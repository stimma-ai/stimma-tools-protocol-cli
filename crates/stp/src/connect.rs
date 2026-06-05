//! Resolve global options + config into a connected, handshaken [`Session`].

use crate::cli::GlobalOpts;
use crate::config::{self, Config, ServerCfg};
use std::collections::HashMap;
use std::path::PathBuf;
use stp_client::error::{Result, StpError};
use stp_client::{connect_websocket, spawn_stdio, Session, StdioConfig};
use url::Url;

enum Target {
    Ws { url: String, token: Option<String> },
    Stdio { cfg: StdioConfig },
}

fn parse_env(pairs: &[String]) -> HashMap<String, String> {
    pairs
        .iter()
        .filter_map(|p| {
            p.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect()
}

fn to_anyhow(e: anyhow::Error) -> StpError {
    StpError::runtime(e)
}

fn build_stdio(
    exec: &str,
    cwd: Option<&str>,
    extra_env: HashMap<String, String>,
) -> Result<StdioConfig> {
    let parts = shlex::split(exec)
        .ok_or_else(|| StpError::usage(format!("could not parse --exec command: {exec:?}")))?;
    let mut it = parts.into_iter();
    let program = it
        .next()
        .ok_or_else(|| StpError::usage("--exec command is empty"))?;
    Ok(StdioConfig {
        program,
        args: it.collect(),
        cwd: cwd.map(PathBuf::from),
        env: extra_env,
    })
}

fn resolve_target(global: &GlobalOpts, cfg: &Config) -> Result<Target> {
    let cli_env = parse_env(&global.env);

    // 1. explicit one-off flags
    if let Some(url) = &global.url {
        let token = config::resolve_token(global.token.as_deref(), None).map_err(to_anyhow)?;
        return Ok(Target::Ws {
            url: url.clone(),
            token,
        });
    }
    if let Some(exec) = &global.exec {
        return Ok(Target::Stdio {
            cfg: build_stdio(exec, global.cwd.as_deref(), cli_env)?,
        });
    }

    // 2. -s/--server alias, else STP_SERVER, else default_server
    let alias = global
        .server
        .clone()
        .or_else(|| std::env::var("STP_SERVER").ok().filter(|s| !s.is_empty()))
        .or_else(|| cfg.default_server.clone());

    if let Some(name) = alias {
        let server = cfg.servers.get(&name).ok_or_else(|| {
            StpError::usage(format!(
                "no server alias {name:?} in config (try `stp config list`)"
            ))
        })?;
        return target_from_server(global, server, cli_env);
    }

    // 3. bare env URL/EXEC
    if let Ok(url) = std::env::var("STP_URL") {
        if !url.is_empty() {
            let token = config::resolve_token(global.token.as_deref(), None).map_err(to_anyhow)?;
            return Ok(Target::Ws { url, token });
        }
    }
    if let Ok(exec) = std::env::var("STP_EXEC") {
        if !exec.is_empty() {
            return Ok(Target::Stdio {
                cfg: build_stdio(&exec, global.cwd.as_deref(), cli_env)?,
            });
        }
    }

    Err(StpError::usage(
        "no server selected: pass -s <alias>, --url, or --exec (or set a default_server)",
    ))
}

fn target_from_server(
    global: &GlobalOpts,
    server: &ServerCfg,
    cli_env: HashMap<String, String>,
) -> Result<Target> {
    if let Some(exec) = &server.exec {
        let mut env = parse_env(
            &server
                .env
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>(),
        );
        env.extend(cli_env);
        let cwd = global.cwd.clone().or_else(|| server.cwd.clone());
        Ok(Target::Stdio {
            cfg: build_stdio(exec, cwd.as_deref(), env)?,
        })
    } else if let Some(url) = &server.url {
        let token =
            config::resolve_token(global.token.as_deref(), Some(server)).map_err(to_anyhow)?;
        Ok(Target::Ws {
            url: url.clone(),
            token,
        })
    } else {
        Err(StpError::usage("server alias has neither url nor exec"))
    }
}

/// Connect and complete the registration handshake.
pub async fn connect(global: &GlobalOpts, cfg: &Config) -> Result<Session> {
    let target = resolve_target(global, cfg)?;
    let conn = match target {
        Target::Ws { url, token } => {
            // Accept http(s):// too — silently use the WebSocket scheme.
            let url = config::normalize_ws_url(&url);
            let parsed =
                Url::parse(&url).map_err(|e| StpError::usage(format!("bad url {url:?}: {e}")))?;
            connect_websocket(parsed, token).await?
        }
        Target::Stdio { cfg } => spawn_stdio(cfg).await?,
    };
    Session::handshake(conn).await
}
