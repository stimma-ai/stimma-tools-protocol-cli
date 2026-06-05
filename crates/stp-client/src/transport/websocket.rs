//! WebSocket transport: connect as a client, optionally authenticated, and pump text frames.

use super::{AssetConfig, Connection, TransportGuard};
use crate::error::{Result, StpError};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{HeaderValue, AUTHORIZATION};
use tokio_tungstenite::tungstenite::Message;
use url::Url;

/// Connect to a provider's WebSocket server. The provider speaks first (`provider.register`),
/// so the reader is live before the session sends its reply.
pub async fn connect_websocket(ws_url: Url, token: Option<String>) -> Result<Connection> {
    let mut request = ws_url
        .as_str()
        .into_client_request()
        .map_err(|e| StpError::connection(format!("bad ws url: {e}")))?;
    if let Some(tok) = &token {
        let val = HeaderValue::from_str(&format!("Bearer {tok}"))
            .map_err(|e| StpError::connection(format!("bad token: {e}")))?;
        request.headers_mut().insert(AUTHORIZATION, val);
    }

    let (ws_stream, _resp) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| StpError::connection(format!("connecting to {ws_url}: {e}")))?;

    let (mut sink, mut stream) = ws_stream.split();

    let (out_tx, mut out_rx) = mpsc::channel::<String>(256);
    let (in_tx, in_rx) = mpsc::channel::<String>(256);

    // Reader: ws text frames -> inbound.
    let reader = tokio::spawn(async move {
        while let Some(msg) = stream.next().await {
            match msg {
                Ok(Message::Text(t)) => {
                    if in_tx.send(t.to_string()).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Binary(b)) => {
                    if let Ok(s) = String::from_utf8(b.to_vec()) {
                        if in_tx.send(s).await.is_err() {
                            break;
                        }
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    });

    // Writer: outbound -> ws text frames.
    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if sink.send(Message::text(msg)).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    Ok(Connection {
        outbound: out_tx,
        inbound: in_rx,
        asset_config: AssetConfig::Http { ws_url, token },
        guard: TransportGuard {
            tasks: vec![reader, writer],
            child: None,
            tempdir: None,
        },
    })
}
