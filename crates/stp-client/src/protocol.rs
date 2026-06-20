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

impl ProviderRegister {
    /// The provider software's product identity parsed from `server`, or `None` when the
    /// field is absent or doesn't parse as `Name/Version` (hosts treat that as an unknown
    /// product; the registration is still accepted).
    pub fn server_identity(&self) -> Option<ServerIdentity> {
        self.server.as_deref().and_then(parse_server_field)
    }
}

/// Product identity carried by the registration's `server` field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerIdentity {
    /// Fixed product name of the provider implementation (e.g. `ComfyUI-Stimma`).
    pub product_name: String,
    /// The provider software's own version (e.g. `1.2.3` or `1.2.0-beta1`).
    pub product_version: String,
}

/// Parse a registration `server` value per the spec's `Name/Version` format:
/// `Name` is a fixed product identifier (no whitespace, no `/`); `Version` is dot-separated
/// numbers with an optional `-suffix`. Returns `None` for anything else.
pub fn parse_server_field(value: &str) -> Option<ServerIdentity> {
    let (name, version) = value.split_once('/')?;
    if name.is_empty() || name.contains(char::is_whitespace) || version.contains('/') {
        return None;
    }
    let (numbers, suffix) = match version.split_once('-') {
        Some((n, s)) => (n, Some(s)),
        None => (version, None),
    };
    let numbers_ok = !numbers.is_empty()
        && numbers
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    if !numbers_ok {
        return None;
    }
    if let Some(s) = suffix {
        if s.is_empty() || s.contains(char::is_whitespace) {
            return None;
        }
    }
    Some(ServerIdentity {
        product_name: name.to_string(),
        product_version: version.to_string(),
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parsed(s: &str) -> Option<(String, String)> {
        parse_server_field(s).map(|i| (i.product_name, i.product_version))
    }

    #[test]
    fn server_field_valid() {
        assert_eq!(
            parsed("ComfyUI-Stimma/1.2.3"),
            Some(("ComfyUI-Stimma".into(), "1.2.3".into()))
        );
        assert_eq!(
            parsed("stp-mock-provider/0.1.0"),
            Some(("stp-mock-provider".into(), "0.1.0".into()))
        );
        assert_eq!(parsed("Tool/7"), Some(("Tool".into(), "7".into())));
    }

    #[test]
    fn server_field_suffixed_version() {
        assert_eq!(
            parsed("ComfyUI-Stimma/1.2.0-beta1"),
            Some(("ComfyUI-Stimma".into(), "1.2.0-beta1".into()))
        );
        assert_eq!(parsed("X/0.9-rc.2"), Some(("X".into(), "0.9-rc.2".into())));
    }

    #[test]
    fn server_field_garbage() {
        for bad in [
            "",
            "ComfyUI-Stimma",         // no slash
            "/1.2.3",                 // empty name
            "ComfyUI-Stimma/",        // empty version
            "Comfy UI/1.2.3",         // whitespace in name
            "a/b/1.2.3",              // extra slash
            "Tool/v1.2.3",            // non-numeric version
            "Tool/1..2",              // empty dot segment
            "Tool/1.2.3-",            // empty suffix
            "Tool/1.2.3-beta 1",      // whitespace in suffix
            "Mozilla/5.0 (like STP)", // trailing junk
        ] {
            assert_eq!(parsed(bad), None, "expected {bad:?} to be rejected");
        }
    }

    #[test]
    fn server_identity_absent_is_none() {
        let reg: ProviderRegister = serde_json::from_value(json!({ "provider_id": "p" })).unwrap();
        assert!(reg.server.is_none());
        assert!(reg.server_identity().is_none());
    }
}
