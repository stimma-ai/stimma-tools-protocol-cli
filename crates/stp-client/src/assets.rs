//! Asset-ID grammar and out-of-band transfer (WebSocket HTTP + stdio filesystem).
//!
//! Host rules distilled from the protocol (SPEC §6.1):
//! - validate every provider-supplied id before building a path/URL (path-traversal guard);
//! - attach the bearer token only to **same-origin** asset requests; never to a cross-origin
//!   endpoint or signed URL;
//! - delete **output** assets only, after downloading all of them.

use crate::error::{Result, StpError};
use rand::Rng;
use std::path::PathBuf;
use url::Url;

/// Asset-ID grammar: `^[A-Za-z0-9_-]{8,64}\.[A-Za-z0-9]{1,12}$`.
pub fn valid_asset_id(id: &str) -> bool {
    let Some(dot) = id.find('.') else {
        return false;
    };
    let (body, ext_with_dot) = id.split_at(dot);
    let ext = &ext_with_dot[1..];
    if ext.contains('.') {
        return false; // exactly one dot
    }
    let body_ok = (8..=64).contains(&body.len())
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    let ext_ok = (1..=12).contains(&ext.len()) && ext.bytes().all(|b| b.is_ascii_alphanumeric());
    body_ok && ext_ok
}

/// Reject an id that doesn't match the grammar.
pub fn check_asset_id(id: &str) -> Result<()> {
    if valid_asset_id(id) {
        Ok(())
    } else {
        Err(StpError::protocol(format!(
            "asset id {id:?} does not match the STP grammar"
        )))
    }
}

/// Mint a fresh, grammar-valid asset id with the given extension (no leading dot).
pub fn new_asset_id(ext: &str) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::rng();
    let body: String = (0..24)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect();
    let ext: String = ext
        .trim_start_matches('.')
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(12)
        .collect();
    let ext = if ext.is_empty() { "bin".into() } else { ext };
    format!("{body}.{ext}")
}

/// Mint a fresh host-side request id: `req-` + 24 url-safe chars. Distinct from the asset-id
/// grammar — it's only ever echoed back in progress/result frames, never used as a path.
pub fn new_request_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::rng();
    let body: String = (0..24)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect();
    format!("req-{body}")
}

/// Where assets live for this session.
pub enum AssetStore {
    /// HTTP asset endpoint reachable over the network.
    Http(HttpAssetStore),
    /// Shared filesystem directory (stdio transport `ASSET_PATH`).
    Filesystem(PathBuf),
}

pub struct HttpAssetStore {
    client: reqwest::Client,
    /// Fully-resolved base, e.g. `https://host:port/assets` (no trailing slash).
    base: Url,
    token: Option<String>,
    /// Origin of the control-channel WebSocket; auth is attached only when the asset request
    /// shares this origin.
    ws_origin: Origin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    scheme: String,
    host: String,
    port: Option<u16>,
}

impl Origin {
    pub fn of(url: &Url) -> Origin {
        Origin {
            scheme: http_scheme(url.scheme()).to_string(),
            host: url.host_str().unwrap_or_default().to_string(),
            port: url.port_or_known_default(),
        }
    }
}

fn http_scheme(s: &str) -> &str {
    match s {
        "wss" | "https" => "https",
        _ => "http",
    }
}

impl HttpAssetStore {
    /// Build from the WebSocket URL and the provider's `asset_endpoint`.
    pub fn new(ws_url: &Url, asset_endpoint: &str, token: Option<String>) -> Result<Self> {
        let ws_origin = Origin::of(ws_url);
        let base =
            if asset_endpoint.starts_with("http://") || asset_endpoint.starts_with("https://") {
                Url::parse(asset_endpoint)
                    .map_err(|e| StpError::connection(format!("bad asset_endpoint: {e}")))?
            } else {
                // Relative path → same origin as the WebSocket.
                let mut u = ws_url.clone();
                u.set_scheme(http_scheme(ws_url.scheme()))
                    .map_err(|_| StpError::connection("cannot derive asset origin"))?;
                u.set_path(asset_endpoint.trim_end_matches('/'));
                u.set_query(None);
                u.set_fragment(None);
                u
            };
        Ok(HttpAssetStore {
            client: reqwest::Client::new(),
            base,
            token,
            ws_origin,
        })
    }

    fn url_for(&self, asset_id: &str) -> Result<Url> {
        check_asset_id(asset_id)?;
        let mut u = self.base.clone();
        // Append `/asset_id` to the base path.
        let path = format!("{}/{}", self.base.path().trim_end_matches('/'), asset_id);
        u.set_path(&path);
        Ok(u)
    }

    /// Attach the token only when the request shares the WS origin.
    fn maybe_auth(&self, req: reqwest::RequestBuilder, url: &Url) -> reqwest::RequestBuilder {
        match &self.token {
            Some(tok) if Origin::of(url) == self.ws_origin => {
                req.header("Authorization", format!("Bearer {tok}"))
            }
            _ => req,
        }
    }

    pub async fn put(&self, asset_id: &str, bytes: Vec<u8>) -> Result<()> {
        let url = self.url_for(asset_id)?;
        let req = self.maybe_auth(self.client.put(url.clone()), &url);
        let resp = req
            .body(bytes)
            .send()
            .await
            .map_err(|e| StpError::connection(format!("PUT {asset_id}: {e}")))?;
        if !resp.status().is_success() {
            return Err(StpError::connection(format!(
                "PUT {asset_id} -> HTTP {}",
                resp.status()
            )));
        }
        Ok(())
    }

    pub async fn get(&self, asset_id: &str) -> Result<Vec<u8>> {
        let url = self.url_for(asset_id)?;
        let req = self.maybe_auth(self.client.get(url.clone()), &url);
        let resp = req
            .send()
            .await
            .map_err(|e| StpError::connection(format!("GET {asset_id}: {e}")))?;
        if !resp.status().is_success() {
            return Err(StpError::connection(format!(
                "GET {asset_id} -> HTTP {}",
                resp.status()
            )));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| StpError::connection(format!("GET {asset_id} body: {e}")))?;
        Ok(bytes.to_vec())
    }

    pub async fn delete(&self, asset_id: &str) -> Result<()> {
        let url = self.url_for(asset_id)?;
        let req = self.maybe_auth(self.client.delete(url.clone()), &url);
        // Best-effort, idempotent.
        let _ = req.send().await;
        Ok(())
    }
}

impl AssetStore {
    pub async fn put(&self, asset_id: &str, bytes: Vec<u8>) -> Result<()> {
        check_asset_id(asset_id)?;
        match self {
            AssetStore::Http(h) => h.put(asset_id, bytes).await,
            AssetStore::Filesystem(dir) => {
                let path = dir.join(asset_id);
                tokio::fs::write(path, bytes).await.map_err(Into::into)
            }
        }
    }

    pub async fn get(&self, asset_id: &str) -> Result<Vec<u8>> {
        check_asset_id(asset_id)?;
        match self {
            AssetStore::Http(h) => h.get(asset_id).await,
            AssetStore::Filesystem(dir) => {
                let path = dir.join(asset_id);
                tokio::fs::read(path).await.map_err(Into::into)
            }
        }
    }

    pub async fn delete(&self, asset_id: &str) -> Result<()> {
        if check_asset_id(asset_id).is_err() {
            return Ok(());
        }
        match self {
            AssetStore::Http(h) => h.delete(asset_id).await,
            AssetStore::Filesystem(dir) => {
                let _ = tokio::fs::remove_file(dir.join(asset_id)).await;
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_accepts_valid_ids() {
        assert!(valid_asset_id("abcdef12.png"));
        assert!(valid_asset_id("AbC_1234-5678.jpeg"));
        assert!(valid_asset_id(&format!("{}.bin", "a".repeat(64))));
    }

    #[test]
    fn grammar_rejects_bad_ids() {
        assert!(!valid_asset_id("short.png")); // body < 8
        assert!(!valid_asset_id("../../etc/passwd")); // traversal, no valid dot-ext
        assert!(!valid_asset_id("abcdef12.png/extra"));
        assert!(!valid_asset_id("abcdef12")); // no ext
        assert!(!valid_asset_id("abc.def.png")); // two dots
        assert!(!valid_asset_id("abcdef12.")); // empty ext
        assert!(!valid_asset_id(&format!("{}.bin", "a".repeat(65)))); // body too long
        assert!(!valid_asset_id("abcd ef12.png")); // space
    }

    #[test]
    fn minted_ids_are_valid() {
        for ext in ["png", ".jpeg", "safetensors123456", ""] {
            let id = new_asset_id(ext);
            assert!(valid_asset_id(&id), "minted invalid id: {id}");
        }
    }
}
