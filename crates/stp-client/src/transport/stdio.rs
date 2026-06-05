//! Stdio transport: spawn the provider as a child, frame NDJSON over its stdin/stdout, and
//! continuously drain its stderr so a full pipe can't deadlock it.

use super::{AssetConfig, Connection, TransportGuard};
use crate::error::{Result, StpError};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// How to launch a stdio provider.
pub struct StdioConfig {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: HashMap<String, String>,
}

/// Spawn the provider, wiring channels and an owned temp asset directory (`ASSET_PATH`).
pub async fn spawn_stdio(cfg: StdioConfig) -> Result<Connection> {
    let tempdir = tempfile::Builder::new()
        .prefix("stp-assets-")
        .tempdir()
        .map_err(|e| StpError::runtime(format!("creating asset dir: {e}")))?;
    let asset_dir = tempdir.path().to_path_buf();

    let mut cmd = tokio::process::Command::new(&cfg.program);
    cmd.args(&cfg.args)
        .env("ASSET_PATH", &asset_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = &cfg.cwd {
        cmd.current_dir(dir);
    }
    for (k, v) in &cfg.env {
        cmd.env(k, v);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| StpError::connection(format!("spawning {:?}: {e}", cfg.program)))?;

    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    let (out_tx, mut out_rx) = mpsc::channel::<String>(256);
    let (in_tx, in_rx) = mpsc::channel::<String>(256);

    // Reader: provider stdout (NDJSON) -> inbound. BufReader handles partial reads / lines
    // split across chunks.
    let reader = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if in_tx.send(trimmed.to_string()).await.is_err() {
                break;
            }
        }
    });

    // Writer: outbound -> provider stdin, one JSON object per line.
    let writer = tokio::spawn(async move {
        let mut stdin = stdin;
        while let Some(msg) = out_rx.recv().await {
            let line = format!("{}\n", msg.trim_end());
            if stdin.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            let _ = stdin.flush().await;
        }
    });

    // Drain stderr into the tracing sink (provider logs surface under -v).
    let errdrain = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(target: "provider", "{line}");
        }
    });

    Ok(Connection {
        outbound: out_tx,
        inbound: in_rx,
        asset_config: AssetConfig::Filesystem { dir: asset_dir },
        guard: TransportGuard {
            tasks: vec![reader, writer, errdrain],
            child: Some(child),
            tempdir: Some(tempdir),
        },
    })
}
