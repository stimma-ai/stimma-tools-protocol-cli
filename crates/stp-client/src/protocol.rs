//! STP wire types: JSON-RPC 2.0 envelopes and STP message payloads.
//!
//! Mirrors `stimma/docs/TOOLS_PROTOCOL.md`. `stp` is the **host** (the JSON-RPC client that
//! connects, replies to the provider's `provider.register`, then drives `tools.*`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol version this host speaks.
pub const STP_VERSION: &str = "1.0";

/// Host software version, reported in the registration response (`host_version`).
pub const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A JSON-RPC id. STP uses integers for request ids in practice; we accept either.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Int(i64),
    Str(String),
}

// ---- Generic JSON-RPC frames ------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Request {
    pub jsonrpc: &'static str,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    pub id: Id,
}

impl Request {
    pub fn new(method: impl Into<String>, params: Option<Value>, id: Id) -> Self {
        Request {
            jsonrpc: "2.0",
            method: method.into(),
            params,
            id,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Notification {
    pub jsonrpc: &'static str,
    pub method: String,
    pub params: Value,
}

impl Notification {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Notification {
            jsonrpc: "2.0",
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub result: Value,
    pub id: Id,
}

impl Response {
    pub fn ok(id: Id, result: Value) -> Self {
        Response {
            jsonrpc: "2.0",
            result,
            id,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcErrorObj {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

/// Any inbound frame, demultiplexed by shape.
#[derive(Debug, Clone)]
pub enum Inbound {
    /// A response to a request we sent (`id` present, `result`/`error`).
    Response {
        id: Id,
        result: Option<Value>,
        error: Option<JsonRpcErrorObj>,
    },
    /// A request from the provider (`id` present, `method` present). For the host this is
    /// effectively just `provider.register`.
    Request {
        id: Id,
        method: String,
        params: Value,
    },
    /// A notification (no `id`).
    Notification { method: String, params: Value },
}

/// Parse one line/frame of JSON into an [`Inbound`].
pub fn parse_inbound(text: &str) -> Result<Inbound, serde_json::Error> {
    let v: Value = serde_json::from_str(text)?;
    let id = v.get("id").and_then(parse_id);
    let method = v.get("method").and_then(|m| m.as_str()).map(String::from);

    match (id, method) {
        (Some(id), None) => {
            // Response: has id, no method.
            let error = v
                .get("error")
                .filter(|e| !e.is_null())
                .map(|e| serde_json::from_value::<JsonRpcErrorObj>(e.clone()))
                .transpose()?;
            let result = v.get("result").cloned();
            Ok(Inbound::Response { id, result, error })
        }
        (Some(id), Some(method)) => Ok(Inbound::Request {
            id,
            method,
            params: v.get("params").cloned().unwrap_or(Value::Null),
        }),
        (None, Some(method)) => Ok(Inbound::Notification {
            method,
            params: v.get("params").cloned().unwrap_or(Value::Null),
        }),
        (None, None) => Err(serde::de::Error::custom("frame has neither id nor method")),
    }
}

fn parse_id(v: &Value) -> Option<Id> {
    match v {
        Value::Number(n) => n.as_i64().map(Id::Int),
        Value::String(s) => Some(Id::Str(s.clone())),
        _ => None,
    }
}

// ---- STP payloads -----------------------------------------------------------

/// `provider.register` params (sent by the provider, received by us).
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderRegister {
    #[serde(default)]
    pub stp_version: Option<String>,
    pub provider_id: String,
    #[serde(default)]
    pub provider_name: Option<String>,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default = "default_concurrency")]
    pub max_concurrent: u32,
    #[serde(default)]
    pub asset_endpoint: Option<String>,
    #[serde(default)]
    pub capabilities: Capabilities,
}

fn default_concurrency() -> u32 {
    1
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub cancel: bool,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// Registration response we send back to the provider.
#[derive(Debug, Clone, Serialize)]
pub struct RegistrationResponse {
    pub session_id: String,
    pub stp_version: &'static str,
    pub host_version: &'static str,
    pub capabilities: Value,
}

impl RegistrationResponse {
    pub fn new(session_id: String) -> Self {
        RegistrationResponse {
            session_id,
            stp_version: STP_VERSION,
            host_version: HOST_VERSION,
            // `stp` advertises no optional host-side features today — but we say so
            // explicitly with an empty object rather than omitting the field.
            capabilities: Value::Object(Default::default()),
        }
    }
}

/// One produced/consumed asset in a `tools.result` output envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub asset_id: String,
    #[serde(rename = "type", default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
}

impl Asset {
    /// Role used for selection, defaulting to `"primary"` per spec.
    pub fn role_or_primary(&self) -> &str {
        self.role.as_deref().unwrap_or("primary")
    }
}

/// `tools.progress` notification params.
#[derive(Debug, Clone, Deserialize)]
pub struct Progress {
    pub request_id: String,
    pub progress: f64,
}

/// `queue.status` notification params.
#[derive(Debug, Clone, Deserialize)]
pub struct QueueStatus {
    #[serde(default)]
    pub queued: u32,
    #[serde(default)]
    pub running: u32,
    #[serde(default)]
    pub capacity: u32,
}

/// `tools.result` notification params (terminal).
#[derive(Debug, Clone, Deserialize)]
pub struct JobResult {
    pub request_id: String,
    pub success: bool,
    #[serde(default)]
    pub output: Option<JobOutput>,
    #[serde(default)]
    pub error: Option<JobError>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JobOutput {
    #[serde(default)]
    pub assets: Vec<Asset>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JobError {
    pub code: String,
    #[serde(default)]
    pub message: String,
}
