//! YAML config: server aliases, token resolution, and writes.
//!
//! Reads via serde (robust). Writes (`config add/remove/set-default`) round-trip the typed
//! model and re-emit with a managed header comment. (Comment-preserving in-place edits via a
//! rowan YAML editor are deferred until that ecosystem matures; `config init` + `config edit`
//! cover hand-tuning meanwhile.)

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_server: Option<String>,
    #[serde(default)]
    pub servers: BTreeMap<String, ServerCfg>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerCfg {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_command: Option<String>,
}

impl ServerCfg {
    /// How a token is stored, for `config get` display (never the value).
    pub fn token_source(&self) -> &'static str {
        if self.token_env.is_some() {
            "env"
        } else if self.token_command.is_some() {
            "command"
        } else if self.token.is_some() {
            "literal"
        } else {
            "none"
        }
    }

    pub fn kind(&self) -> &'static str {
        if self.exec.is_some() {
            "stdio"
        } else {
            "websocket"
        }
    }
}

/// Rewrite an `http(s)://` URL to its WebSocket scheme so users can paste either form.
pub fn normalize_ws_url(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        url.to_string()
    }
}

/// Resolve the config file path: `--config` / `STP_CONFIG` / XDG `stp/config.yaml`.
pub fn config_path(override_path: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = override_path {
        return Ok(PathBuf::from(p));
    }
    if let Ok(p) = std::env::var("STP_CONFIG") {
        return Ok(PathBuf::from(p));
    }
    use etcetera::BaseStrategy;
    let strat = etcetera::choose_base_strategy().context("locating config dir")?;
    Ok(strat.config_dir().join("stp").join("config.yaml"))
}

pub fn load(path: &PathBuf) -> Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let cfg: Config =
        serde_yaml_ng::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(cfg)
}

/// Resolve a bearer token (first wins): CLI flag, STP_TOKEN, then the alias's source.
pub fn resolve_token(
    cli_token: Option<&str>,
    server: Option<&ServerCfg>,
) -> Result<Option<String>> {
    if let Some(t) = cli_token {
        return Ok(Some(t.to_string()));
    }
    if let Ok(t) = std::env::var("STP_TOKEN") {
        if !t.is_empty() {
            return Ok(Some(t));
        }
    }
    let Some(s) = server else {
        return Ok(None);
    };
    if let Some(var) = &s.token_env {
        return Ok(std::env::var(var).ok().filter(|v| !v.is_empty()));
    }
    if let Some(cmd) = &s.token_command {
        let out = run_token_command(cmd)?;
        return Ok(Some(out));
    }
    Ok(s.token.clone())
}

fn run_token_command(cmd: &str) -> Result<String> {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .with_context(|| format!("running token_command: {cmd}"))?;
    if !output.status.success() {
        bail!("token_command failed: {cmd}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

// ---- writes (comment-preserving via yaml-edit) ------------------------------

const TEMPLATE: &str = r#"# ~/.config/stp/config.yaml — Stimma Tools Protocol CLI
#
# `default_server` is used when no -s/--server flag and no STP_SERVER env are given.
default_server: cloud

# Each entry under `servers:` is an alias you select with `stp -s <name> …`.
servers:
  # A WebSocket provider (cloud or remote box).
  cloud:
    url: wss://cloud.stimma.ai/stp-v1
    token_env: STIMMA_TOKEN        # read the bearer token from this env var (preferred)
    # token_command: pass show stimma/cloud   # …or run a command to fetch it
    # token: "sk-…"                            # …or inline (discouraged: plaintext secret)

  # A stdio provider (stp spawns it as a subprocess; no network, no token).
  # localcomfy:
  #   exec: python -m comfyui_provider
  #   cwd: /srv/comfy
  #   env:
  #     CUDA_VISIBLE_DEVICES: "0"
"#;

pub fn init(path: &PathBuf) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, TEMPLATE)?;
    Ok(true)
}

const HEADER: &str = "# ~/.config/stp/config.yaml — Stimma Tools Protocol CLI\n\
    # Managed by `stp config`. Edit by hand with `stp config edit`.\n\
    # Each entry under `servers:` is an alias selected with `stp -s <name> …`.\n\n";

fn save(path: &PathBuf, cfg: &Config) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_yaml_ng::to_string(cfg).context("serializing config")?;
    std::fs::write(path, format!("{HEADER}{body}"))?;
    Ok(())
}

pub struct AddSpec {
    pub name: String,
    pub url: Option<String>,
    pub exec: Option<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    pub token: Option<String>,
    pub token_env: Option<String>,
    pub token_command: Option<String>,
}

pub fn add(path: &PathBuf, spec: &AddSpec) -> Result<()> {
    if spec.url.is_none() && spec.exec.is_none() {
        bail!("an alias needs either --url or --exec");
    }
    let mut cfg = load(path)?;
    cfg.servers.insert(
        spec.name.clone(),
        ServerCfg {
            url: spec.url.as_deref().map(normalize_ws_url),
            exec: spec.exec.clone(),
            cwd: spec.cwd.clone(),
            env: spec.env.iter().cloned().collect(),
            token: spec.token.clone(),
            token_env: spec.token_env.clone(),
            token_command: spec.token_command.clone(),
        },
    );
    save(path, &cfg)
}

pub fn remove(path: &PathBuf, name: &str) -> Result<()> {
    let mut cfg = load(path)?;
    if cfg.servers.remove(name).is_none() {
        bail!("no such server alias: {name}");
    }
    if cfg.default_server.as_deref() == Some(name) {
        cfg.default_server = None;
    }
    save(path, &cfg)
}

pub fn set_default(path: &PathBuf, name: &str) -> Result<()> {
    let mut cfg = load(path)?;
    if !cfg.servers.contains_key(name) {
        bail!("no such server alias: {name}");
    }
    cfg.default_server = Some(name.to_string());
    save(path, &cfg)
}
