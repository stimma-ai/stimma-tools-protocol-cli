//! Command implementations.

use crate::agent_guide;
use crate::cli::{
    AgentGuideArgs, Cli, Command, CompletionsArgs, ConfigAddArgs, ConfigCmd, GlobalOpts, RawArgs,
    RunArgs, ShowArgs, ToolsArgs, WatchArgs,
};
use crate::config::{self, AddSpec};
use crate::ctx::Ctx;
use crate::params::{self, OutputMode, ParsedRun};
use crate::render;
use clap::CommandFactory;
use serde_json::{json, Value};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use stp_client::assets::{new_asset_id, new_request_id};
use stp_client::descriptor::ToolDescriptor;
use stp_client::error::{Result, StpError};
use stp_client::protocol::{Progress, QueueStatus};
use stp_client::session::{RawResponse, Session};

// ---- no-connection commands -------------------------------------------------

pub fn version() {
    println!(
        "stp {} (STP protocol {})",
        env!("CARGO_PKG_VERSION"),
        stp_client::protocol::STP_VERSION
    );
}

pub fn completions(args: CompletionsArgs) -> Result<()> {
    let mut cmd = Cli::command();
    clap_complete::generate(args.shell, &mut cmd, "stp", &mut std::io::stdout());
    Ok(())
}

pub fn agent_guide(args: AgentGuideArgs) -> Result<()> {
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&agent_guide::manifest())?
        );
    } else {
        print!("{}", agent_guide::MARKDOWN);
    }
    Ok(())
}

fn to_stp(e: anyhow::Error) -> StpError {
    StpError::runtime(e)
}

pub fn config_cmd(global: &GlobalOpts, cmd: ConfigCmd) -> Result<()> {
    let path = config::config_path(global.config.as_deref()).map_err(to_stp)?;
    match cmd {
        ConfigCmd::Path => println!("{}", path.display()),
        ConfigCmd::Init => {
            if config::init(&path).map_err(to_stp)? {
                println!("wrote template config to {}", path.display());
            } else {
                println!("config already exists at {}", path.display());
            }
        }
        ConfigCmd::List => {
            let cfg = config::load(&path).map_err(to_stp)?;
            let mut raw = String::from("ALIAS\tKIND\tTARGET\tTOKEN\tDEFAULT\n");
            for (name, s) in &cfg.servers {
                let target = s.url.clone().or_else(|| s.exec.clone()).unwrap_or_default();
                let def = if cfg.default_server.as_deref() == Some(name) {
                    "*"
                } else {
                    ""
                };
                raw.push_str(&format!(
                    "{name}\t{}\t{target}\t{}\t{def}\n",
                    s.kind(),
                    s.token_source()
                ));
            }
            print!("{}", render::tabify_public(&raw));
        }
        ConfigCmd::Get { name } => {
            let cfg = config::load(&path).map_err(to_stp)?;
            let s = cfg
                .servers
                .get(&name)
                .ok_or_else(|| StpError::usage(format!("no such alias: {name}")))?;
            println!("alias    {name}");
            println!("kind     {}", s.kind());
            if let Some(u) = &s.url {
                println!("url      {u}");
            }
            if let Some(e) = &s.exec {
                println!("exec     {e}");
            }
            if let Some(c) = &s.cwd {
                println!("cwd      {c}");
            }
            println!("token    {} (value not shown)", s.token_source());
        }
        ConfigCmd::Add(a) => {
            let spec = add_spec(a);
            config::add(&path, &spec).map_err(to_stp)?;
            println!("added server '{}'", spec.name);
        }
        ConfigCmd::Remove { name } => {
            config::remove(&path, &name).map_err(to_stp)?;
            println!("removed server '{name}'");
        }
        ConfigCmd::SetDefault { name } => {
            config::set_default(&path, &name).map_err(to_stp)?;
            println!("default server is now '{name}'");
        }
        ConfigCmd::Edit => {
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
            config::init(&path).map_err(to_stp)?;
            let status = std::process::Command::new(editor).arg(&path).status()?;
            if !status.success() {
                return Err(StpError::runtime("editor exited non-zero"));
            }
        }
    }
    Ok(())
}

fn add_spec(a: ConfigAddArgs) -> AddSpec {
    AddSpec {
        name: a.name,
        url: a.url,
        exec: a.exec,
        cwd: a.cwd,
        env: a
            .env
            .iter()
            .filter_map(|p| {
                p.split_once('=')
                    .map(|(k, v)| (k.to_string(), v.to_string()))
            })
            .collect(),
        token: a.token,
        token_env: a.token_env,
        token_command: a.token_command,
    }
}

// ---- connected dispatch -----------------------------------------------------

pub async fn run_connected(global: &GlobalOpts, ctx: &Ctx, command: Command) -> Result<()> {
    let path = config::config_path(global.config.as_deref()).map_err(to_stp)?;
    let cfg = config::load(&path).map_err(to_stp)?;
    let mut session = crate::connect::connect(global, &cfg).await?;

    match command {
        Command::Tools(a) => cmd_tools(&mut session, ctx, a).await,
        Command::Show(a) => cmd_show(&mut session, ctx, a).await,
        Command::Raw(a) => cmd_raw(&mut session, a).await,
        Command::Watch(a) => cmd_watch(&mut session, a).await,
        Command::Run(a) => cmd_run(&mut session, ctx, a).await,
        _ => unreachable!("non-connected command routed here"),
    }
}

async fn list_value(session: &mut Session) -> Result<Value> {
    session.request("tools.list", json!({})).await
}

fn parse_tools(v: &Value) -> Result<Vec<ToolDescriptor>> {
    let arr = v
        .get("tools")
        .cloned()
        .ok_or_else(|| StpError::protocol("tools.list missing `tools`"))?;
    serde_json::from_value(arr).map_err(|e| StpError::protocol(format!("bad descriptors: {e}")))
}

async fn cmd_tools(session: &mut Session, ctx: &Ctx, a: ToolsArgs) -> Result<()> {
    let mut value = list_value(session).await?;
    if let Some(task) = &a.task {
        if let Some(arr) = value.get_mut("tools").and_then(|t| t.as_array_mut()) {
            arr.retain(|t| {
                t.get("task_types")
                    .and_then(|tt| tt.as_array())
                    .map(|tt| tt.iter().any(|x| x.as_str() == Some(task.as_str())))
                    .unwrap_or(false)
            });
        }
    }
    if a.json {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    let tools = parse_tools(&value)?;
    if a.format.as_deref() == Some("tsv") {
        println!("{}", render::tools_tsv(&tools));
    } else {
        println!(
            "{}",
            render::tools_table(&tools, &render::Palette::new(ctx.color))
        );
    }
    Ok(())
}

async fn find_descriptor(session: &mut Session, tool: &str) -> Result<(Value, ToolDescriptor)> {
    let value = list_value(session).await?;
    let raw = value
        .get("tools")
        .and_then(|t| t.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|t| t.get("id").and_then(|i| i.as_str()) == Some(tool))
        })
        .cloned()
        .ok_or_else(|| StpError::usage(format!("no such tool: {tool} (try `stp tools`)")))?;
    let desc: ToolDescriptor = serde_json::from_value(raw.clone())
        .map_err(|e| StpError::protocol(format!("bad descriptor: {e}")))?;
    Ok((raw, desc))
}

async fn cmd_show(session: &mut Session, ctx: &Ctx, a: ShowArgs) -> Result<()> {
    let pal = render::Palette::new(ctx.color);
    match a.tool {
        // `show <tool>` — tool detail or schema dump.
        Some(tool) => {
            let (raw, desc) = find_descriptor(session, &tool).await?;
            if a.json {
                println!("{}", serde_json::to_string_pretty(&raw)?);
            } else if a.schema_only {
                println!("{}", serde_json::to_string_pretty(&desc.parameter_schema)?);
            } else if a.output_schema {
                println!("{}", serde_json::to_string_pretty(&desc.output_schema)?);
            } else {
                println!("{}", render::show_tool(&desc, &pal));
            }
        }
        // `show` — provider info + queue snapshot.
        None => {
            // Spec: `server` is required and MUST parse as `Name/Version`; hosts still accept
            // registrations without it and treat the product identity as unknown.
            match session.provider.server_identity() {
                Some(_) => {}
                None => match &session.provider.server {
                    Some(s) => ctx.note(format!(
                        "warning: `server` value {s:?} does not parse as `Name/Version`; \
                         product identity is unknown"
                    )),
                    None => ctx.note(
                        "warning: registration has no `server` field (required as \
                         `Name/Version`); product identity is unknown",
                    ),
                },
            }
            let value = list_value(session).await?;
            let count = value
                .get("tools")
                .and_then(|t| t.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            let p = &session.provider;
            if a.json {
                let out = json!({
                    "provider_id": p.provider_id,
                    "provider_name": p.provider_name,
                    "server": p.server,
                    "stp_version": p.stp_version,
                    "transport": session.transport,
                    "asset_endpoint": session.asset_endpoint,
                    "max_concurrent": p.max_concurrent,
                    "capabilities": { "cancel": p.capabilities.cancel },
                    "tools": count,
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                let cap = p.max_concurrent;
                println!(
                    "{}",
                    render::show_server(session, count, Some((0, 0, cap)), &pal)
                );
            }
        }
    }
    Ok(())
}

async fn cmd_raw(session: &mut Session, a: RawArgs) -> Result<()> {
    let params = match a.params.as_deref() {
        None => Value::Object(Default::default()),
        Some("-") => {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
            serde_json::from_str(&s)?
        }
        Some(p) if p.starts_with('@') => {
            let text = std::fs::read_to_string(&p[1..])?;
            serde_json::from_str(&text)?
        }
        Some(p) => serde_json::from_str(p)
            .map_err(|e| StpError::usage(format!("params is not valid JSON: {e}")))?,
    };
    match session.request_raw(&a.method, params).await? {
        RawResponse::Result(v) => println!("{}", serde_json::to_string_pretty(&v)?),
        RawResponse::Error { code, message } => {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"error": {"code": code, "message": message}})
                )?
            );
            return Err(StpError::JsonRpc { code, message });
        }
    }
    Ok(())
}

async fn cmd_watch(session: &mut Session, a: WatchArgs) -> Result<()> {
    let json = a.json;
    let pump = session.pump_notifications(|method, params| {
        if json {
            let frame = json!({"method": method, "params": params});
            println!("{frame}");
        } else {
            print!("{method}");
            match method {
                "queue.status" => {
                    println!(
                        "      queued={} running={} capacity={}",
                        params.get("queued").unwrap_or(&json!(0)),
                        params.get("running").unwrap_or(&json!(0)),
                        params.get("capacity").unwrap_or(&json!(0)),
                    );
                }
                "tools.progress" => {
                    println!(
                        "    job={} {}",
                        params
                            .get("request_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?"),
                        params.get("progress").unwrap_or(&json!(0)),
                    );
                }
                _ => println!("    {params}"),
            }
        }
        let _ = std::io::stdout().flush();
    });
    tokio::select! {
        r = pump => r,
        _ = tokio::signal::ctrl_c() => Err(StpError::Interrupted),
    }
}

// ---- run --------------------------------------------------------------------

enum Outcome {
    Done(Result<stp_client::protocol::JobResult>),
    Interrupted,
    Timeout,
}

async fn cmd_run(session: &mut Session, ctx: &Ctx, a: RunArgs) -> Result<()> {
    let RunArgs { tool, rest } = a;
    let (_raw, desc) = find_descriptor(session, &tool).await?;
    let mut parsed = params::parse_run(&desc, &rest)?;

    let request_id = parsed.request_id.clone().unwrap_or_else(new_request_id);

    // Build the upload plan (forced inputs + auto-detected file paths).
    let plan = build_upload_plan(&desc, &parsed);

    if parsed.dry_run {
        // Preview without uploading or executing. Auto-detected files are already in params;
        // only the explicit --input bindings need inserting for the preview.
        for (param, file) in &parsed.forced_inputs {
            push_media(
                &mut parsed.params,
                &desc,
                param,
                Value::String(file.clone()),
            );
        }
        let preview = json!({
            "tool_id": tool,
            "request_id": request_id,
            "parameters": parsed.params,
            "upload_plan": plan.iter().map(|(p, f, _)| json!({"param": p, "file": f}))
                .collect::<Vec<_>>(),
        });
        // Validate too, so dry-run is a real preflight.
        validate(&desc, &parsed)?;
        println!("{}", serde_json::to_string_pretty(&preview)?);
        return Ok(());
    }

    // Validate before spending uploads/execution.
    validate(&desc, &parsed)?;

    // Upload inputs once; the same asset ids are reused across every run in a batch.
    for (param, file, _array) in &plan {
        let bytes = std::fs::read(file)
            .map_err(|e| StpError::usage(format!("reading input {file}: {e}")))?;
        let ext = Path::new(file)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin");
        let id = new_asset_id(ext);
        session.asset_store.put(&id, bytes).await?;
        ctx.note(format!("uploading {file} → {id}"));
        replace_or_push_media(&mut parsed.params, &desc, param, file, &id);
    }

    let count = parsed.count;
    let multi = count > 1;
    if multi && matches!(parsed.output, OutputMode::Stdout) {
        return Err(StpError::usage(
            "--stdout can't be combined with -n/--count",
        ));
    }
    if multi && matches!(parsed.output, OutputMode::File(_)) {
        return Err(StpError::usage(
            "-o <file> can't be combined with -n/--count (use a directory, or omit -o)",
        ));
    }

    let show_pb = std::io::IsTerminal::is_terminal(&std::io::stderr())
        && !matches!(parsed.output, OutputMode::Stdout)
        && !parsed.json;
    let stem_base = format!("gen_{}", chrono::Local::now().format("%Y%m%d-%H%M%S"));

    let mut results: Vec<Value> = Vec::new();
    for i in 0..count {
        let req_id = if multi {
            format!("{request_id}-{}", i + 1)
        } else {
            request_id.clone()
        };
        let stem = if multi {
            format!("{stem_base}_{}", i + 1)
        } else {
            stem_base.clone()
        };
        let prefix = if show_pb {
            Some(if multi {
                format!("[{}/{count}] ", i + 1)
            } else {
                String::new()
            })
        } else {
            None
        };

        let result = execute_one(
            session,
            &tool,
            Value::Object(parsed.params.clone()),
            &req_id,
            parsed.timeout,
            prefix,
        )
        .await?;

        if !result.success {
            let err = result.error.unwrap_or(stp_client::protocol::JobError {
                code: "EXECUTION_FAILED".into(),
                message: "job failed".into(),
            });
            return Err(StpError::Job {
                code: err.code,
                message: err.message,
            });
        }

        let assets = result
            .output
            .as_ref()
            .map(|o| o.assets.clone())
            .unwrap_or_default();
        let written = deliver_outputs(session, &parsed, &assets, &stem).await?;
        if !parsed.json {
            for p in &written {
                println!("{p}");
            }
        }
        let mut record = json!({
            "request_id": req_id,
            "success": true,
            "output": { "assets": assets.iter().map(|x| json!({
                "asset_id": x.asset_id, "type": x.r#type, "role": x.role,
            })).collect::<Vec<_>>() },
            "paths": written,
        });
        // Only surface `metadata` when the provider actually sent some — an empty `{}` reads
        // like a bug.
        if let Some(meta) = result.metadata.filter(|m| !is_empty_object(m)) {
            record["metadata"] = meta;
        }
        results.push(record);
    }

    if parsed.json {
        if multi {
            println!("{}", serde_json::to_string_pretty(&Value::Array(results))?);
        } else {
            println!("{}", serde_json::to_string_pretty(&results[0])?);
        }
    }
    Ok(())
}

/// Run one execution: progress bar, Ctrl-C/timeout handling, returns the terminal result.
async fn execute_one(
    session: &mut Session,
    tool: &str,
    params_value: Value,
    request_id: &str,
    timeout: Option<u64>,
    pb_prefix: Option<String>,
) -> Result<stp_client::protocol::JobResult> {
    let pb = pb_prefix.map(|prefix| {
        let bar = indicatif::ProgressBar::new(100);
        bar.set_style(
            indicatif::ProgressStyle::with_template(
                "{prefix}generating {bar:20} {percent}% {elapsed}",
            )
            .unwrap(),
        );
        bar.set_prefix(prefix);
        bar
    });
    let outcome = {
        let mut on_progress = |p: &Progress| {
            if let Some(bar) = &pb {
                bar.set_position((p.progress * 100.0) as u64);
            }
        };
        let mut on_queue = |_q: &QueueStatus| {};
        let exec = session.execute(
            request_id,
            tool,
            params_value,
            &mut on_progress,
            &mut on_queue,
        );
        tokio::pin!(exec);
        let sleeper = async {
            match timeout {
                Some(s) => tokio::time::sleep(std::time::Duration::from_secs(s)).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(sleeper);
        tokio::select! {
            r = &mut exec => Outcome::Done(r),
            _ = tokio::signal::ctrl_c() => Outcome::Interrupted,
            _ = &mut sleeper => Outcome::Timeout,
        }
    };
    if let Some(bar) = &pb {
        bar.finish_and_clear();
    }
    match outcome {
        Outcome::Done(r) => r,
        Outcome::Interrupted => {
            session.cancel(request_id).await?;
            Err(StpError::Interrupted)
        }
        Outcome::Timeout => {
            session.cancel(request_id).await?;
            Err(StpError::Timeout(std::time::Duration::from_secs(
                timeout.unwrap_or(0),
            )))
        }
    }
}

fn validate(desc: &ToolDescriptor, parsed: &ParsedRun) -> Result<()> {
    if parsed.no_validate {
        return Ok(());
    }
    if !parsed.allow_unknown {
        let unknown = stp_client::schema::unknown_keys(&desc.parameter_schema, &parsed.params);
        if !unknown.is_empty() {
            return Err(StpError::Validation(format!(
                "  unknown parameter(s): {} (use --allow-unknown to pass through)",
                unknown.join(", ")
            )));
        }
    }
    stp_client::schema::validate(
        &desc.parameter_schema,
        &Value::Object(parsed.params.clone()),
    )
}

fn media_array(desc: &ToolDescriptor, param: &str) -> Option<bool> {
    desc.media_params()
        .into_iter()
        .find(|m| m.name == param)
        .map(|m| m.array)
}

/// Whether a parameter value should be uploaded as a local file.
///
/// An existing file on disk always wins over the asset-id grammar: our own output naming
/// (`gen_<ts>.png`) happens to match the grammar, so without this a user who feeds a generated
/// file straight back in (generate → upscale/edit) would have it passed through as a bogus
/// asset id instead of uploaded. Grammar/passthrough only applies to values that are *not* a
/// file on disk.
fn looks_like_file(s: &str) -> bool {
    !s.starts_with("data:") && Path::new(s).is_file()
}

/// Files that will be uploaded: explicit `--input` first, then auto-detected file paths in
/// media params.
fn build_upload_plan(desc: &ToolDescriptor, parsed: &ParsedRun) -> Vec<(String, String, bool)> {
    let mut plan = Vec::new();
    for (param, file) in &parsed.forced_inputs {
        let array = media_array(desc, param).unwrap_or(true);
        plan.push((param.clone(), file.clone(), array));
    }
    if !parsed.no_auto_upload {
        for m in desc.media_params() {
            if let Some(val) = parsed.params.get(&m.name) {
                match val {
                    Value::String(s) if looks_like_file(s) => {
                        plan.push((m.name.clone(), s.clone(), m.array))
                    }
                    Value::Array(arr) => {
                        for v in arr {
                            if let Some(s) = v.as_str() {
                                if looks_like_file(s) {
                                    plan.push((m.name.clone(), s.to_string(), true));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    plan
}

fn push_media(
    params: &mut serde_json::Map<String, Value>,
    desc: &ToolDescriptor,
    param: &str,
    value: Value,
) {
    let array = media_array(desc, param).unwrap_or(true);
    if array {
        let entry = params.entry(param.to_string()).or_insert_with(|| json!([]));
        if let Some(arr) = entry.as_array_mut() {
            arr.push(value);
        }
    } else {
        params.insert(param.to_string(), value);
    }
}

fn replace_or_push_media(
    params: &mut serde_json::Map<String, Value>,
    desc: &ToolDescriptor,
    param: &str,
    file: &str,
    id: &str,
) {
    // Forced inputs aren't in params yet; auto inputs are present as the file path.
    let array = media_array(desc, param).unwrap_or(true);
    match params.get_mut(param) {
        Some(Value::Array(arr)) => {
            let mut replaced = false;
            for v in arr.iter_mut() {
                if v.as_str() == Some(file) {
                    *v = Value::String(id.to_string());
                    replaced = true;
                    break;
                }
            }
            if !replaced {
                arr.push(Value::String(id.to_string()));
            }
        }
        Some(Value::String(s)) if s == file => {
            params.insert(param.to_string(), Value::String(id.to_string()));
        }
        _ => {
            if array {
                let entry = params.entry(param.to_string()).or_insert_with(|| json!([]));
                if let Some(arr) = entry.as_array_mut() {
                    arr.push(Value::String(id.to_string()));
                }
            } else {
                params.insert(param.to_string(), Value::String(id.to_string()));
            }
        }
    }
}

/// Select a single asset by role (or the conventional primary).
fn select_asset<'a>(
    assets: &'a [stp_client::protocol::Asset],
    role: Option<&str>,
) -> Result<&'a stp_client::protocol::Asset> {
    if let Some(r) = role {
        return assets
            .iter()
            .find(|a| a.role_or_primary() == r)
            .ok_or_else(|| roles_err(assets, &format!("no asset with role {r:?}")));
    }
    if assets.len() == 1 {
        return Ok(&assets[0]);
    }
    let primaries: Vec<_> = assets
        .iter()
        .filter(|a| a.role_or_primary() == "primary")
        .collect();
    match primaries.len() {
        1 => Ok(primaries[0]),
        _ => Err(roles_err(assets, "ambiguous asset selection")),
    }
}

fn roles_err(assets: &[stp_client::protocol::Asset], why: &str) -> StpError {
    let roles: Vec<String> = assets
        .iter()
        .map(|a| a.role_or_primary().to_string())
        .collect();
    StpError::usage(format!(
        "{why}; available roles: {} (use --role)",
        roles.join(", ")
    ))
}

fn ext_of(asset_id: &str) -> &str {
    asset_id.rsplit_once('.').map(|(_, e)| e).unwrap_or("bin")
}

/// A path that doesn't collide: `<dir>/<stem>.<ext>`, then `_2`, `_3`, …
fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let mut p = dir.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{stem}_{n}.{ext}"));
        n += 1;
    }
    p
}

/// Download (GET-all) then best-effort DELETE-all of the output assets. Returns written paths.
///
/// Default layout: a single output becomes one file `gen_<ts>[.idx].<ext>`; multiple outputs
/// become a `gen_<ts>[.idx]/` folder with files named by provider role.
async fn deliver_outputs(
    session: &mut Session,
    parsed: &ParsedRun,
    assets: &[stp_client::protocol::Asset],
    stem: &str,
) -> Result<Vec<String>> {
    let mut written = Vec::new();
    match &parsed.output {
        OutputMode::None => {}
        OutputMode::Stdout => {
            let asset = select_asset(assets, parsed.role.as_deref())?;
            let bytes = session.asset_store.get(&asset.asset_id).await?;
            std::io::stdout().write_all(&bytes)?;
        }
        OutputMode::File(path) => {
            let asset = select_asset(assets, parsed.role.as_deref())?;
            let bytes = session.asset_store.get(&asset.asset_id).await?;
            std::fs::write(path, bytes)?;
            written.push(abs_path(path));
        }
        OutputMode::Dir(_) | OutputMode::DefaultDir => {
            let base = match &parsed.output {
                OutputMode::Dir(d) => PathBuf::from(d),
                _ => PathBuf::from("."),
            };
            if assets.len() == 1 {
                // One output: a plain file, no folder.
                std::fs::create_dir_all(&base)?;
                let bytes = session.asset_store.get(&assets[0].asset_id).await?;
                let path = unique_path(&base, stem, ext_of(&assets[0].asset_id));
                std::fs::write(&path, bytes)?;
                written.push(abs_path(&path.to_string_lossy()));
            } else {
                // Multiple outputs: a folder named gen_<ts>, files named by role.
                let dir = unique_dir(&base, stem);
                std::fs::create_dir_all(&dir)?;
                let mut seen: std::collections::HashMap<String, u32> =
                    std::collections::HashMap::new();
                for asset in assets {
                    let bytes = session.asset_store.get(&asset.asset_id).await?;
                    let role = asset.role_or_primary().to_string();
                    let n = seen.entry(role.clone()).or_insert(0);
                    *n += 1;
                    let role_stem = if *n == 1 {
                        role.clone()
                    } else {
                        format!("{role}_{n}")
                    };
                    let path = dir.join(format!("{role_stem}.{}", ext_of(&asset.asset_id)));
                    std::fs::write(&path, bytes)?;
                    written.push(abs_path(&path.to_string_lossy()));
                }
            }
        }
    }

    // DELETE-all only after every GET, and only outputs. Skipped with --keep-remote or when
    // we downloaded nothing.
    if !parsed.keep_remote && !matches!(parsed.output, OutputMode::None) {
        for asset in assets {
            let _ = session.asset_store.delete(&asset.asset_id).await;
        }
    }
    Ok(written)
}

/// A directory that doesn't collide: `<base>/<stem>`, then `<stem>_2`, …
fn unique_dir(base: &Path, stem: &str) -> PathBuf {
    let mut p = base.join(stem);
    let mut n = 2;
    while p.exists() {
        p = base.join(format!("{stem}_{n}"));
        n += 1;
    }
    p
}

/// True for `{}` (or `null`) — a metadata payload not worth surfacing.
fn is_empty_object(v: &Value) -> bool {
    v.as_object().map(|o| o.is_empty()).unwrap_or(true)
}

/// Absolute form of a written path (printing is the caller's job, so `--json` stays clean).
fn abs_path(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string())
}
