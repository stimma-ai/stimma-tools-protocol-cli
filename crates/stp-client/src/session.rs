//! Host session: registration handshake, then request/notification pumping over one transport.
//!
//! A CLI runs one high-level operation at a time, so the session uses a simple "pump until the
//! awaited frame arrives" model rather than a background demux task: notifications that arrive
//! while we wait for a response are dispatched inline. The transport's reader task is already
//! buffering inbound frames into a channel before we send anything, so the provider's
//! unsolicited `provider.register` (which it sends first) is never missed.

use crate::assets::AssetStore;
use crate::descriptor::ToolDescriptor;
use crate::error::{Result, StpError};
use crate::protocol::{
    parse_inbound, Id, Inbound, JobResult, Notification, Progress, ProviderRegister, QueueStatus,
    RegistrationResponse, Request, Response,
};
use crate::transport::Connection;
use serde_json::{json, Value};

pub struct Session {
    conn: Connection,
    next_id: i64,
    pub provider: ProviderRegister,
    pub session_id: String,
    pub asset_store: AssetStore,
    pub asset_endpoint: Option<String>,
    /// Human label for the transport (e.g. `websocket (wss://…)` or `stdio`).
    pub transport: String,
}

/// Outcome of a request: a result value or a JSON-RPC error object.
pub enum RawResponse {
    Result(Value),
    Error { code: i64, message: String },
}

impl Session {
    /// Perform the registration handshake: receive `provider.register`, build the asset store,
    /// reply with the registration response.
    pub async fn handshake(mut conn: Connection) -> Result<Session> {
        let frame = conn
            .inbound
            .recv()
            .await
            .ok_or_else(|| StpError::connection("provider closed before registering"))?;
        let inbound = parse_inbound(&frame)
            .map_err(|e| StpError::protocol(format!("malformed first frame: {e}")))?;

        let (id, params) = match inbound {
            Inbound::Request { id, method, params } if method == "provider.register" => {
                (id, params)
            }
            Inbound::Request { method, .. } => {
                return Err(StpError::protocol(format!(
                    "expected provider.register, got request {method:?}"
                )))
            }
            _ => {
                return Err(StpError::protocol(
                    "expected provider.register as the first frame",
                ))
            }
        };

        let register: ProviderRegister = serde_json::from_value(params)
            .map_err(|e| StpError::protocol(format!("bad provider.register: {e}")))?;
        let asset_endpoint = register.asset_endpoint.clone();
        let transport = match &conn.asset_config {
            crate::transport::AssetConfig::Http { ws_url, .. } => {
                format!("websocket ({ws_url})")
            }
            crate::transport::AssetConfig::Filesystem { .. } => "stdio".to_string(),
        };
        let asset_store = conn.asset_config.build(asset_endpoint.as_deref())?;

        let session_id = format!("stp-{}", std::process::id());
        let resp = Response::ok(
            id,
            serde_json::to_value(RegistrationResponse::new(session_id.clone()))?,
        );
        conn.outbound
            .send(serde_json::to_string(&resp)?)
            .await
            .map_err(|_| StpError::connection("transport closed during handshake"))?;

        Ok(Session {
            conn,
            next_id: 1,
            provider: register,
            session_id,
            asset_store,
            asset_endpoint,
            transport,
        })
    }

    fn alloc_id(&mut self) -> Id {
        let id = self.next_id;
        self.next_id += 1;
        Id::Int(id)
    }

    async fn send(&self, text: String) -> Result<()> {
        tracing::debug!(target: "stp_client::wire", "→ {text}");
        self.conn
            .outbound
            .send(text)
            .await
            .map_err(|_| StpError::connection("transport closed"))
    }

    async fn recv(&mut self) -> Result<String> {
        let frame = self
            .conn
            .inbound
            .recv()
            .await
            .ok_or_else(|| StpError::connection("provider disconnected"))?;
        tracing::debug!(target: "stp_client::wire", "← {frame}");
        Ok(frame)
    }

    /// Reply to a (re-)registration request the provider may send mid-session.
    async fn reply_register(&self, id: Id) -> Result<()> {
        let resp = Response::ok(
            id,
            serde_json::to_value(RegistrationResponse::new(self.session_id.clone()))?,
        );
        self.send(serde_json::to_string(&resp)?).await
    }

    /// Send a request and pump frames until its response arrives. Notifications in between are
    /// ignored. Returns the result value or a mapped JSON-RPC error.
    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        match self.request_raw(method, params).await? {
            RawResponse::Result(v) => Ok(v),
            RawResponse::Error { code, message } => Err(StpError::JsonRpc { code, message }),
        }
    }

    /// Like [`request`], but surfaces a JSON-RPC error as a value rather than an `Err` (used by
    /// `raw`, which prints either).
    pub async fn request_raw(&mut self, method: &str, params: Value) -> Result<RawResponse> {
        let id = self.alloc_id();
        let req = Request::new(method, Some(params), id.clone());
        self.send(serde_json::to_string(&req)?).await?;

        loop {
            let frame = self.recv().await?;
            match parse_inbound(&frame)
                .map_err(|e| StpError::protocol(format!("malformed frame: {e}")))?
            {
                Inbound::Response {
                    id: rid,
                    result,
                    error,
                } if rid == id => {
                    if let Some(err) = error {
                        return Ok(RawResponse::Error {
                            code: err.code,
                            message: err.message,
                        });
                    }
                    return Ok(RawResponse::Result(result.unwrap_or(Value::Null)));
                }
                Inbound::Request { id, method, .. } if method == "provider.register" => {
                    self.reply_register(id).await?;
                }
                _ => { /* stray response/notification: ignore */ }
            }
        }
    }

    /// `tools.list` → descriptors.
    pub async fn list_tools(&mut self) -> Result<Vec<ToolDescriptor>> {
        self.tools_from("tools.list").await
    }

    /// `tools.refresh` → descriptors.
    pub async fn refresh_tools(&mut self) -> Result<Vec<ToolDescriptor>> {
        self.tools_from("tools.refresh").await
    }

    async fn tools_from(&mut self, method: &str) -> Result<Vec<ToolDescriptor>> {
        let result = self.request(method, json!({})).await?;
        let tools = result
            .get("tools")
            .cloned()
            .ok_or_else(|| StpError::protocol(format!("{method} result missing `tools`")))?;
        serde_json::from_value(tools)
            .map_err(|e| StpError::protocol(format!("bad tool descriptors: {e}")))
    }

    /// Execute with ordinary media only; retained for existing client callers.
    pub async fn execute<P, Q>(
        &mut self,
        request_id: &str,
        tool_id: &str,
        parameters: Value,
        on_progress: P,
        on_queue: Q,
    ) -> Result<JobResult>
    where
        P: FnMut(&Progress),
        Q: FnMut(&QueueStatus),
    {
        self.execute_with_sidecars(
            request_id,
            tool_id,
            parameters,
            Value::Null,
            on_progress,
            on_queue,
        )
        .await
    }

    /// Send `tools.execute`, require the `accepted: true` ack, then pump progress/queue
    /// notifications until the terminal `tools.result` for `request_id`.
    pub async fn execute_with_sidecars<P, Q>(
        &mut self,
        request_id: &str,
        tool_id: &str,
        parameters: Value,
        input_sidecars: Value,
        mut on_progress: P,
        mut on_queue: Q,
    ) -> Result<JobResult>
    where
        P: FnMut(&Progress),
        Q: FnMut(&QueueStatus),
    {
        let id = self.alloc_id();
        let mut params =
            json!({"request_id": request_id, "tool_id": tool_id, "parameters": parameters});
        if self
            .provider
            .capabilities
            .extra
            .get("media_sidecars")
            .and_then(Value::as_bool)
            == Some(true)
            && input_sidecars.is_object()
        {
            params["input_sidecars"] = input_sidecars;
        }
        let req = Request::new("tools.execute", Some(params), id.clone());
        self.send(serde_json::to_string(&req)?).await?;

        let mut acked = false;
        loop {
            let frame = self.recv().await?;
            match parse_inbound(&frame)
                .map_err(|e| StpError::protocol(format!("malformed frame: {e}")))?
            {
                Inbound::Response {
                    id: rid,
                    result,
                    error,
                } if rid == id => {
                    if let Some(err) = error {
                        return Err(StpError::JsonRpc {
                            code: err.code,
                            message: err.message,
                        });
                    }
                    let accepted = result
                        .as_ref()
                        .and_then(|r| r.get("accepted"))
                        .and_then(|a| a.as_bool())
                        .unwrap_or(false);
                    if !accepted {
                        return Err(StpError::protocol("provider did not accept the job"));
                    }
                    acked = true;
                }
                Inbound::Notification { method, params } => match method.as_str() {
                    "tools.progress" => {
                        if let Ok(p) = serde_json::from_value::<Progress>(params) {
                            if p.request_id == request_id {
                                on_progress(&p);
                            }
                        }
                    }
                    "queue.status" => {
                        if let Ok(q) = serde_json::from_value::<QueueStatus>(params) {
                            on_queue(&q);
                        }
                    }
                    "tools.result" => {
                        let res: JobResult = serde_json::from_value(params)
                            .map_err(|e| StpError::protocol(format!("bad tools.result: {e}")))?;
                        if res.request_id == request_id {
                            return Ok(res);
                        }
                    }
                    _ => {}
                },
                Inbound::Request { id, method, .. } if method == "provider.register" => {
                    self.reply_register(id).await?;
                }
                _ => {}
            }
            let _ = acked; // ack may arrive before or after the first notification
        }
    }

    /// Best-effort `tools.cancel` (only meaningful if the provider advertised `cancel`).
    pub async fn cancel(&mut self, request_id: &str) -> Result<()> {
        if !self.provider.capabilities.cancel {
            return Ok(());
        }
        let _ = self
            .request("tools.cancel", json!({ "request_id": request_id }))
            .await;
        Ok(())
    }

    /// Graceful disconnect notification.
    pub async fn disconnect(&self, reason: &str) {
        let note = Notification::new("provider.disconnect", json!({ "reason": reason }));
        if let Ok(text) = serde_json::to_string(&note) {
            let _ = self.send(text).await;
        }
    }

    /// Pump notifications indefinitely, invoking `handler` for each (used by `watch`).
    pub async fn pump_notifications<F>(&mut self, mut handler: F) -> Result<()>
    where
        F: FnMut(&str, &Value),
    {
        loop {
            let frame = self.recv().await?;
            match parse_inbound(&frame)
                .map_err(|e| StpError::protocol(format!("malformed frame: {e}")))?
            {
                Inbound::Notification { method, params } => handler(&method, &params),
                Inbound::Request { id, method, .. } if method == "provider.register" => {
                    self.reply_register(id).await?;
                }
                _ => {}
            }
        }
    }

    pub fn capabilities_cancel(&self) -> bool {
        self.provider.capabilities.cancel
    }
}
