//! Transport layer. A [`Connection`] gives the session a pair of channels — outbound text
//! frames in, inbound text frames out — plus the information needed to build the asset store.
//! A [`TransportGuard`] tears everything down on drop (child kill+reap, temp dir removal,
//! reader/writer task abort), so a signal or error never orphans a process or leaks a dir.

mod stdio;
mod websocket;

pub use stdio::{spawn_stdio, StdioConfig};
pub use websocket::connect_websocket;

use crate::assets::AssetStore;
use crate::error::Result;
use std::path::PathBuf;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use url::Url;

/// How to build the [`AssetStore`] once handshake details are known.
pub enum AssetConfig {
    /// WebSocket: HTTP endpoint resolved from the provider's `asset_endpoint`.
    Http { ws_url: Url, token: Option<String> },
    /// Stdio: shared filesystem directory.
    Filesystem { dir: PathBuf },
}

impl AssetConfig {
    /// Build the asset store. `asset_endpoint` is the value from `provider.register`
    /// (ignored for filesystem).
    pub fn build(&self, asset_endpoint: Option<&str>) -> Result<AssetStore> {
        match self {
            AssetConfig::Filesystem { dir } => Ok(AssetStore::Filesystem(dir.clone())),
            AssetConfig::Http { ws_url, token } => {
                let endpoint = asset_endpoint.unwrap_or("/assets");
                Ok(AssetStore::Http(crate::assets::HttpAssetStore::new(
                    ws_url,
                    endpoint,
                    token.clone(),
                )?))
            }
        }
    }
}

/// An established transport.
pub struct Connection {
    pub outbound: mpsc::Sender<String>,
    pub inbound: mpsc::Receiver<String>,
    pub asset_config: AssetConfig,
    pub guard: TransportGuard,
}

/// Owns the resources that must be cleaned up. Dropping aborts tasks, kills/reaps a stdio
/// child, and removes the temp asset dir.
#[derive(Default)]
pub struct TransportGuard {
    pub(crate) tasks: Vec<JoinHandle<()>>,
    pub(crate) child: Option<tokio::process::Child>,
    // Held purely for RAII: dropping removes the temp asset dir.
    #[allow(dead_code)]
    pub(crate) tempdir: Option<TempDir>,
}

impl Drop for TransportGuard {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
        if let Some(child) = self.child.as_mut() {
            // kill_on_drop is set, but be explicit so the reap is prompt.
            let _ = child.start_kill();
        }
        // tempdir removed by its own Drop.
    }
}
