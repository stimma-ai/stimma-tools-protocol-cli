//! `stp sweep` — provider-agnostic smoke test for every advertised tool.
//!
//! For each tool the provider lists, sweep builds the *cheapest valid* parameters from the
//! tool's schema (cost-driving keys pinned to their minimum, everything else left to provider
//! defaults), runs one generation, and checks the output is a plausible asset (correct magic
//! bytes for the task's media kind). Tools that can't be attempted — a required option with no
//! candidates (model not installed), or a required media input with no fixture — are SKIPped,
//! not failed. Any real FAIL makes the command exit non-zero, so it can gate CI/cron later.
//!
//! Nothing here is ComfyUI-specific: it operates purely on STP schemas and results, so it works
//! against any provider. Media fixtures for image-to-image / upscale / video tools come from a
//! manifest hosted by Stimma Cloud (see `--assets-url`), downloaded into a local cache.

use crate::cli::SweepArgs;
use crate::ctx::Ctx;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use stp_client::assets::new_asset_id;
use stp_client::descriptor::ToolDescriptor;
use stp_client::error::{Result, StpError};
use stp_client::protocol::{Asset, JobError, JobResult, Progress, QueueStatus};
use stp_client::session::Session;

/// Fixed, cheap prompt for every text-conditioned tool.
const SWEEP_PROMPT: &str = "a small red cube on a plain white background";
/// Fixed seed for reproducibility.
const SWEEP_SEED: i64 = 42;

/// Parameter names whose value drives cost/time; pinned to the schema minimum.
const COST_KEYS: &[&str] = &[
    "steps",
    "num_inference_steps",
    "sampling_steps",
    "num_steps",
    "width",
    "height",
    "resolution",
    "megapixels",
    "frame_count",
    "frames",
    "num_frames",
    "length",
    "video_length",
    "duration",
    "seconds",
    "fps",
    "frame_rate",
    "num_images",
    "batch_size",
];

// ---- media kinds ------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Image,
    Video,
    Audio,
    #[allow(dead_code)] // reserved for document fixtures/outputs
    Document,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Video => "video",
            Kind::Audio => "audio",
            Kind::Document => "document",
        }
    }
}

/// The output media kind a task is expected to produce.
fn expected_output_kind(desc: &ToolDescriptor) -> Kind {
    let tasks = desc.task_types.join(" ").to_lowercase();
    if tasks.contains("video") {
        Kind::Video
    } else if tasks.contains("audio")
        || tasks.contains("speech")
        || tasks.contains("music")
        || tasks.contains("voice")
    {
        Kind::Audio
    } else {
        Kind::Image
    }
}

/// Map a media `x-control` to the fixture kind it consumes.
///
/// `video_frame_picker` is an *image* input — a single still frame decoded by an image node
/// (e.g. an i2v start frame) — even though the UI lets the user pick that frame from a video.
/// Only `video_picker` consumes a whole video container. Feeding a video container to a frame
/// picker makes the image node fail to decode it.
fn media_input_kind(control: &str) -> Kind {
    match control {
        "video_picker" => Kind::Video,
        "audio_picker" => Kind::Audio,
        _ => Kind::Image, // image_picker, video_frame_picker (still frame), mask_editor
    }
}

/// Fixture kind a named media param consumes.
fn media_kind_of(desc: &ToolDescriptor, name: &str) -> Kind {
    desc.property(name)
        .and_then(|p| p.get("x-control"))
        .and_then(|c| c.as_str())
        .map(media_input_kind)
        .unwrap_or(Kind::Image)
}

/// Slots a media param needs filled: some inputs are positional sets (a stitch
/// takes two clips), so one fixture is not a valid request.
fn media_min_items(desc: &ToolDescriptor, name: &str) -> usize {
    desc.property(name)
        .and_then(|p| p.get("x-min-items").or_else(|| p.get("minItems")))
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as usize
}

/// Minimum this `anyOf` branch demands of a param (branches may tighten it).
fn branch_min_items(branch: &Value, name: &str) -> usize {
    branch
        .get("properties")
        .and_then(|p| p.get(name))
        .and_then(|p| p.get("minItems"))
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as usize
}

// ---- report -----------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Fail,
    Skip,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::Skip => "SKIP",
        }
    }
}

struct ToolReport {
    id: String,
    task: String,
    status: Status,
    duration_ms: Option<u128>,
    output: Option<String>,
    note: String,
    params: Value,
}

// ---- entry point ------------------------------------------------------------

pub async fn cmd_sweep(session: &mut Session, ctx: &Ctx, args: SweepArgs) -> Result<()> {
    let overrides = load_overrides(args.overrides.as_deref())?;

    let mut tools = session.list_tools().await?;
    if !args.only.is_empty() {
        let want = &args.only;
        for id in want {
            if !tools.iter().any(|t| &t.id == id) {
                ctx.note(format!("warning: no such tool {id:?} (skipping)"));
            }
        }
        tools.retain(|t| want.contains(&t.id));
    }
    tools.sort_by(|a, b| a.id.cmp(&b.id));
    if tools.is_empty() {
        return Err(StpError::usage("no tools to sweep"));
    }

    let mut fixtures = Fixtures::new(&args, ctx)?;

    // Plan-only mode: show what would run, no execution.
    if args.list {
        return plan_only(ctx, &tools, &overrides, &mut fixtures).await;
    }

    let out_dir = match &args.out {
        Some(d) => {
            let p = PathBuf::from(d);
            std::fs::create_dir_all(&p).map_err(StpError::from)?;
            Some(p)
        }
        None => None,
    };

    ctx.note(format!("sweeping {} tool(s)…", tools.len()));
    let mut reports = Vec::new();
    for desc in &tools {
        let report = sweep_one(
            session,
            ctx,
            &args,
            &overrides,
            &mut fixtures,
            desc,
            out_dir.as_deref(),
        )
        .await?;
        // live one-liner to stderr as each tool finishes
        ctx.note(format!(
            "  {:<5} {}{}",
            report.status.word(),
            report.id,
            if report.note.is_empty() {
                String::new()
            } else {
                format!("  — {}", report.note)
            }
        ));
        reports.push(report);
    }

    print_table(&reports);
    if let Some(path) = &args.report {
        write_json_report(path, session, &reports)?;
        ctx.note(format!("wrote report to {path}"));
    }

    let passed = reports.iter().filter(|r| r.status == Status::Pass).count();
    let failed = reports.iter().filter(|r| r.status == Status::Fail).count();
    let skipped = reports.iter().filter(|r| r.status == Status::Skip).count();
    println!("\n{passed} passed, {failed} failed, {skipped} skipped");

    if failed > 0 {
        return Err(StpError::Job {
            code: "SWEEP_FAILED".into(),
            message: format!("{failed} of {} tool(s) failed", reports.len()),
        });
    }
    Ok(())
}

// ---- per-tool plan ----------------------------------------------------------

/// What to do with one tool: run it (with built params + media uploads) or skip it.
enum Plan {
    Run {
        params: Map<String, Value>,
        /// (param name, local fixture path, array-valued, slots to fill) to
        /// upload before executing. A multi-slot input repeats the same asset.
        media: Vec<(String, PathBuf, bool, usize)>,
    },
    Skip(String),
}

async fn build_plan(
    desc: &ToolDescriptor,
    overrides: &BTreeMap<String, Map<String, Value>>,
    fixtures: &mut Fixtures<'_>,
) -> Plan {
    let required = required_names(desc);
    let media_params = desc.media_params();
    let media_names: Vec<&str> = media_params.iter().map(|m| m.name.as_str()).collect();

    // Resolve fixtures for required media inputs (optional ones are omitted entirely).
    let mut media = Vec::new();
    for m in &media_params {
        if !required.contains(&m.name) {
            continue;
        }
        let kind = media_kind_of(desc, &m.name);
        match fixtures.resolve(kind).await {
            // A multi-slot input (e.g. a stitch needing two clips) declares
            // `x-min-items`; sending one fixture fails validation on the
            // provider, so repeat the fixture up to the declared minimum.
            Some(path) => media.push((m.name.clone(), path, m.array, media_min_items(desc, &m.name))),
            None => {
                return Plan::Skip(format!(
                    "no {} fixture for required input {:?}",
                    kind.label(),
                    m.name
                ))
            }
        }
    }

    // Cross-field alternatives: a tool may accept several kinds of reference
    // input where each is individually OPTIONAL but at least one is required
    // (`anyOf`). Nothing above fills those, so satisfy the cheapest branch we
    // have a fixture for — otherwise the request is rejected before it runs.
    if let Some(branches) = desc
        .parameter_schema
        .get("anyOf")
        .and_then(|v| v.as_array())
    {
        let branch_names = |b: &Value| -> Vec<String> {
            b.get("required")
                .and_then(|r| r.as_array())
                .map(|r| r.iter().filter_map(|n| n.as_str().map(String::from)).collect())
                .unwrap_or_default()
        };
        let satisfied = branches.iter().any(|b| {
            let names = branch_names(b);
            !names.is_empty() && names.iter().all(|n| media.iter().any(|(mn, _, _, _)| mn == n))
        });
        if !satisfied {
            let mut filled = false;
            // Prefer an image branch: the cheapest fixture and the fastest run.
            let mut ordered: Vec<&Value> = branches.iter().collect();
            ordered.sort_by_key(|b| {
                branch_names(b)
                    .first()
                    .map(|n| match media_kind_of(desc, n) {
                        Kind::Image => 0,
                        Kind::Video => 1,
                        _ => 2,
                    })
                    .unwrap_or(3)
            });
            'branches: for b in ordered {
                let names = branch_names(b);
                if names.is_empty() {
                    continue;
                }
                let mut staged = Vec::new();
                for n in &names {
                    if !media_names.contains(&n.as_str()) {
                        continue 'branches; // not a media param — can't satisfy generically
                    }
                    match fixtures.resolve(media_kind_of(desc, n)).await {
                        Some(path) => staged.push((
                            n.clone(),
                            path,
                            media_params.iter().find(|m| &m.name == n).map(|m| m.array).unwrap_or(true),
                            branch_min_items(b, n).max(media_min_items(desc, n)),
                        )),
                        None => continue 'branches,
                    }
                }
                media.extend(staged);
                filled = true;
                break;
            }
            if !filled {
                return Plan::Skip(
                    "no fixture satisfies any of the tool's alternative input requirements"
                        .to_string(),
                );
            }
        }
    }

    // Pre-flight: a required choice param with no candidates means the option (e.g. a model
    // checkpoint) isn't installed on this provider — skip rather than fail.
    if let Some(props) = desc
        .parameter_schema
        .get("properties")
        .and_then(|p| p.as_object())
    {
        for (name, prop) in props {
            if !required.contains(name) || media_names.contains(&name.as_str()) {
                continue;
            }
            if let Some(empty_enum) = empty_choice(prop) {
                return Plan::Skip(format!(
                    "required param {name:?} has no {empty_enum} (model/option not installed?)"
                ));
            }
        }
    }

    // Build scalar parameters.
    let mut params = Map::new();
    if let Some(props) = desc
        .parameter_schema
        .get("properties")
        .and_then(|p| p.as_object())
    {
        for (name, prop) in props {
            if media_names.contains(&name.as_str()) {
                continue; // handled via fixtures below
            }
            let control = prop.get("x-control").and_then(|c| c.as_str()).unwrap_or("");
            let ty = prop.get("type").and_then(|t| t.as_str()).unwrap_or("");

            let lname = name.to_lowercase();
            if lname.contains("negative") {
                // Leave negative prompts to their default — filling them with the subject would
                // suppress exactly what we asked for. Set empty only if the schema requires it.
                if required.contains(name) {
                    params.insert(name.clone(), json!(""));
                }
            } else if name == "prompt" || control == "prompt_editor" {
                if ty == "string" || ty.is_empty() {
                    params.insert(name.clone(), json!(SWEEP_PROMPT));
                }
            } else if control == "seed" || lname.contains("seed") {
                if ty == "integer" {
                    params.insert(name.clone(), json!(SWEEP_SEED));
                }
            } else if is_cost_key(name) {
                if let Some(v) = min_value(prop) {
                    params.insert(name.clone(), v);
                }
            } else if required.contains(name) {
                if let Some(v) = default_or_synth(prop) {
                    params.insert(name.clone(), v);
                }
            }
            // else: not required, not a cost key → rely on the provider's default.
        }
    }

    // Insert fixture file paths into media params (validated as strings; replaced with asset
    // ids at upload time).
    for (name, path, array, min_items) in &media {
        let s = Value::String(path.to_string_lossy().to_string());
        if *array {
            let n = (*min_items).max(1);
            params.insert(name.clone(), Value::Array(vec![s; n]));
        } else {
            params.insert(name.clone(), s);
        }
    }

    // Apply per-tool overrides last (escape hatch when schema-min produces a bad request).
    if let Some(ov) = overrides.get(&desc.id) {
        for (k, v) in ov {
            params.insert(k.clone(), v.clone());
        }
    }

    Plan::Run { params, media }
}

async fn sweep_one(
    session: &mut Session,
    ctx: &Ctx,
    args: &SweepArgs,
    overrides: &BTreeMap<String, Map<String, Value>>,
    fixtures: &mut Fixtures<'_>,
    desc: &ToolDescriptor,
    out_dir: Option<&Path>,
) -> Result<ToolReport> {
    let task = desc.task_types.first().cloned().unwrap_or_default();
    let plan = build_plan(desc, overrides, fixtures).await;
    let (mut params, media) = match plan {
        Plan::Skip(reason) => {
            return Ok(ToolReport {
                id: desc.id.clone(),
                task,
                status: Status::Skip,
                duration_ms: None,
                output: None,
                note: reason,
                params: Value::Null,
            })
        }
        Plan::Run { params, media } => (params, media),
    };

    // Validate the built request before spending uploads/compute. A failure here is our
    // builder falling short of the schema — surface it as a FAIL so it's visible.
    if let Err(e) =
        stp_client::schema::validate(&desc.parameter_schema, &Value::Object(params.clone()))
    {
        return Ok(report_fail(
            desc,
            task,
            None,
            format!("could not build valid params: {}", one_line(&e.to_string())),
            &params,
        ));
    }

    // Upload fixtures, swapping the file path for a fresh asset id.
    for (name, file, array, min_items) in &media {
        let bytes = std::fs::read(file)
            .map_err(|e| StpError::usage(format!("reading fixture {}: {e}", file.display())))?;
        let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("bin");
        let id = new_asset_id(ext);
        session.asset_store.put(&id, bytes).await?;
        if *array {
            let n = (*min_items).max(1);
            params.insert(name.clone(), Value::Array(vec![Value::String(id); n]));
        } else {
            params.insert(name.clone(), json!(id));
        }
    }

    let request_id = stp_client::assets::new_request_id();
    let started = Instant::now();
    let outcome = run_tool(session, ctx, desc, &params, &request_id, args.timeout).await;
    let dur = started.elapsed().as_millis();

    match outcome {
        RunOutcome::Interrupted => Err(StpError::Interrupted),
        RunOutcome::Fatal(e) => Err(e),
        RunOutcome::Timeout => Ok(report_fail(
            desc,
            task,
            Some(dur),
            format!("timed out after {}s", args.timeout),
            &params,
        )),
        RunOutcome::Failed(je) => {
            if is_missing_resource(&je, desc) {
                Ok(ToolReport {
                    id: desc.id.clone(),
                    task,
                    status: Status::Skip,
                    duration_ms: Some(dur),
                    output: None,
                    note: format!("missing resource: {}", one_line(&job_err_msg(&je))),
                    params: Value::Object(params),
                })
            } else {
                Ok(report_fail(
                    desc,
                    task,
                    Some(dur),
                    format!("job error: {}", one_line(&job_err_msg(&je))),
                    &params,
                ))
            }
        }
        RunOutcome::Ok(jr) => {
            let assets = jr
                .output
                .as_ref()
                .map(|o| o.assets.clone())
                .unwrap_or_default();
            validate_output(session, desc, &task, dur, &assets, out_dir, &params).await
        }
    }
}

fn report_fail(
    desc: &ToolDescriptor,
    task: String,
    dur: Option<u128>,
    note: String,
    params: &Map<String, Value>,
) -> ToolReport {
    ToolReport {
        id: desc.id.clone(),
        task,
        status: Status::Fail,
        duration_ms: dur,
        output: None,
        note,
        params: Value::Object(params.clone()),
    }
}

// ---- execution --------------------------------------------------------------

enum RunOutcome {
    Ok(JobResult),
    Failed(JobError),
    Timeout,
    Interrupted,
    /// Transport/protocol break — abort the whole sweep.
    Fatal(StpError),
}

async fn run_tool(
    session: &mut Session,
    ctx: &Ctx,
    desc: &ToolDescriptor,
    params: &Map<String, Value>,
    request_id: &str,
    timeout: u64,
) -> RunOutcome {
    let show_pb = !ctx.quiet && std::io::IsTerminal::is_terminal(&std::io::stderr());
    let pb = show_pb.then(|| {
        let bar = indicatif::ProgressBar::new(100);
        bar.set_style(
            indicatif::ProgressStyle::with_template("  {prefix} {bar:20} {percent}% {elapsed}")
                .unwrap(),
        );
        bar.set_prefix(desc.id.clone());
        bar
    });

    let params_value = Value::Object(params.clone());
    let outcome = {
        let mut on_progress = |p: &Progress| {
            if let Some(bar) = &pb {
                bar.set_position((p.progress * 100.0) as u64);
            }
        };
        let mut on_queue = |_q: &QueueStatus| {};
        let exec = session.execute(
            request_id,
            &desc.id,
            params_value,
            &mut on_progress,
            &mut on_queue,
        );
        tokio::pin!(exec);
        let sleeper = async {
            if timeout == 0 {
                std::future::pending::<()>().await
            } else {
                tokio::time::sleep(Duration::from_secs(timeout)).await
            }
        };
        tokio::pin!(sleeper);
        tokio::select! {
            r = &mut exec => Done::Result(r),
            _ = tokio::signal::ctrl_c() => Done::Interrupted,
            _ = &mut sleeper => Done::Timeout,
        }
    };
    if let Some(bar) = &pb {
        bar.finish_and_clear();
    }

    match outcome {
        Done::Result(Ok(jr)) => {
            if jr.success {
                RunOutcome::Ok(jr)
            } else {
                RunOutcome::Failed(jr.error.unwrap_or(JobError {
                    code: "EXECUTION_FAILED".into(),
                    message: "job failed".into(),
                }))
            }
        }
        // A provider JSON-RPC error rejecting one job is per-tool; transport breaks are fatal.
        Done::Result(Err(e @ StpError::JsonRpc { .. })) => RunOutcome::Failed(JobError {
            code: "JSONRPC_ERROR".into(),
            message: e.to_string(),
        }),
        Done::Result(Err(e)) => RunOutcome::Fatal(e),
        Done::Timeout => {
            let _ = session.cancel(request_id).await;
            RunOutcome::Timeout
        }
        Done::Interrupted => {
            let _ = session.cancel(request_id).await;
            RunOutcome::Interrupted
        }
    }
}

enum Done {
    Result(Result<JobResult>),
    Timeout,
    Interrupted,
}

// ---- output validation ------------------------------------------------------

async fn validate_output(
    session: &mut Session,
    desc: &ToolDescriptor,
    task: &str,
    dur: u128,
    assets: &[Asset],
    out_dir: Option<&Path>,
    params: &Map<String, Value>,
) -> Result<ToolReport> {
    let mk_fail = |note: String| ToolReport {
        id: desc.id.clone(),
        task: task.to_string(),
        status: Status::Fail,
        duration_ms: Some(dur),
        output: None,
        note,
        params: Value::Object(params.clone()),
    };

    let Some(asset) = select_primary(assets) else {
        return Ok(mk_fail("success but no output assets".into()));
    };
    let bytes = match session.asset_store.get(&asset.asset_id).await {
        Ok(b) => b,
        Err(e) => {
            return Ok(mk_fail(format!(
                "could not fetch output: {}",
                one_line(&e.to_string())
            )))
        }
    };
    if bytes.is_empty() {
        return Ok(mk_fail("output asset is empty".into()));
    }

    let expected = expected_output_kind(desc);
    let sniffed = sniff(&bytes);
    let note = match sniffed {
        None => format!(
            "output is not a recognized {} ({})",
            expected.label(),
            fmt_bytes(bytes.len())
        ),
        Some((kind, _fmt)) if kind != expected => format!(
            "expected {} output, got {} ({})",
            expected.label(),
            kind.label(),
            fmt_bytes(bytes.len())
        ),
        Some(_) => String::new(),
    };
    let status = if note.is_empty() {
        Status::Pass
    } else {
        Status::Fail
    };

    // Build the human "output" summary (kind + dims + size).
    let output = sniffed.map(|(kind, fmt)| {
        let dims = dimensions(fmt, &bytes)
            .map(|(w, h)| format!(" {w}x{h}"))
            .unwrap_or_default();
        format!("{}{} {}", kind.label(), dims, fmt_bytes(bytes.len()))
    });

    // Keep the output if requested, then clean up the provider-side asset.
    if status == Status::Pass {
        if let Some(dir) = out_dir {
            let ext = sniffed.map(|(_, f)| f).unwrap_or("bin");
            let path = dir.join(format!("{}.{ext}", desc.id));
            if let Err(e) = std::fs::write(&path, &bytes) {
                return Ok(mk_fail(format!("writing output: {e}")));
            }
        }
    }
    for a in assets {
        let _ = session.asset_store.delete(&a.asset_id).await;
    }

    Ok(ToolReport {
        id: desc.id.clone(),
        task: task.to_string(),
        status,
        duration_ms: Some(dur),
        output,
        note,
        params: Value::Object(params.clone()),
    })
}

fn select_primary(assets: &[Asset]) -> Option<&Asset> {
    if assets.len() == 1 {
        return assets.first();
    }
    assets
        .iter()
        .find(|a| a.role_or_primary() == "primary")
        .or_else(|| assets.first())
}

/// Recognize a byte blob by magic bytes. Mirrors the Cloud tool-health `checkOutputAsset`.
fn sniff(b: &[u8]) -> Option<(Kind, &'static str)> {
    if b.len() >= 8 && b[0..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A] {
        return Some((Kind::Image, "png"));
    }
    if b.len() >= 3 && b[0..3] == [0xFF, 0xD8, 0xFF] {
        return Some((Kind::Image, "jpg"));
    }
    if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return Some((Kind::Image, "webp"));
    }
    if b.len() >= 6 && (&b[0..6] == b"GIF87a" || &b[0..6] == b"GIF89a") {
        return Some((Kind::Image, "gif"));
    }
    if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WAVE" {
        return Some((Kind::Audio, "wav"));
    }
    if b.len() >= 12 && &b[4..8] == b"ftyp" {
        return Some((Kind::Video, "mp4"));
    }
    if b.len() >= 4 && b[0..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        return Some((Kind::Video, "webm"));
    }
    if b.len() >= 4 && &b[0..4] == b"OggS" {
        return Some((Kind::Audio, "ogg"));
    }
    if b.len() >= 3 && &b[0..3] == b"ID3" {
        return Some((Kind::Audio, "mp3"));
    }
    if b.len() >= 2 && b[0] == 0xFF && (b[1] & 0xE0) == 0xE0 {
        return Some((Kind::Audio, "mp3"));
    }
    None
}

/// Best-effort pixel dimensions for common still-image formats (records that cheap params took
/// effect). Video dims are intentionally not probed — no ffprobe dependency.
fn dimensions(fmt: &str, b: &[u8]) -> Option<(u32, u32)> {
    match fmt {
        "png" if b.len() >= 24 => Some((
            u32::from_be_bytes([b[16], b[17], b[18], b[19]]),
            u32::from_be_bytes([b[20], b[21], b[22], b[23]]),
        )),
        "gif" if b.len() >= 10 => Some((
            u16::from_le_bytes([b[6], b[7]]) as u32,
            u16::from_le_bytes([b[8], b[9]]) as u32,
        )),
        "jpg" => jpeg_dimensions(b),
        _ => None,
    }
}

fn jpeg_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 9 < b.len() {
        if b[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = b[i + 1];
        // SOF0..SOF15, excluding DHT(C4), JPG(C8), DAC(CC).
        if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
            let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32;
            let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32;
            return Some((w, h));
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if len < 2 {
            return None;
        }
        i += 2 + len;
    }
    None
}

// ---- schema helpers ---------------------------------------------------------

fn required_names(desc: &ToolDescriptor) -> Vec<String> {
    desc.parameter_schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn is_cost_key(name: &str) -> bool {
    let n = name.to_lowercase();
    COST_KEYS.contains(&n.as_str())
}

/// If a property constrains values to a choice list that is *empty*, return the noun for the
/// list ("options"); else None. Signals a model/checkpoint that isn't installed.
fn empty_choice(prop: &Value) -> Option<&'static str> {
    for key in ["enum", "x-allowed-values"] {
        if let Some(arr) = prop.get(key).and_then(|v| v.as_array()) {
            if arr.is_empty() {
                return Some("options");
            }
        }
    }
    None
}

/// The cheapest legal value for a cost-driving parameter.
fn min_value(prop: &Value) -> Option<Value> {
    // Enumerated numeric choices → the smallest.
    for key in ["x-allowed-values", "enum"] {
        if let Some(arr) = prop.get(key).and_then(|v| v.as_array()) {
            let min = arr.iter().filter(|v| v.as_f64().is_some()).min_by(|a, b| {
                a.as_f64()
                    .unwrap()
                    .partial_cmp(&b.as_f64().unwrap())
                    .unwrap()
            });
            if let Some(v) = min {
                return Some(v.clone());
            }
        }
    }
    if let Some(m) = prop.get("minimum") {
        return Some(m.clone());
    }
    prop.get("default").cloned()
}

/// A value for a required non-cost parameter: prefer the schema default, else synthesize the
/// cheapest legal value for its type.
fn default_or_synth(prop: &Value) -> Option<Value> {
    if let Some(d) = prop.get("default") {
        return Some(d.clone());
    }
    if let Some(arr) = prop.get("enum").and_then(|v| v.as_array()) {
        if let Some(first) = arr.first() {
            return Some(first.clone());
        }
    }
    match prop.get("type").and_then(|t| t.as_str()) {
        Some("integer") | Some("number") => {
            Some(prop.get("minimum").cloned().unwrap_or_else(|| json!(0)))
        }
        Some("boolean") => Some(json!(false)),
        Some("string") => Some(json!("")),
        Some("array") => Some(json!([])),
        Some("object") => Some(json!({})),
        _ => None,
    }
}

// ---- missing-resource classification ---------------------------------------

fn job_err_msg(je: &JobError) -> String {
    if je.message.is_empty() {
        je.code.clone()
    } else {
        format!("{}: {}", je.code, je.message)
    }
}

/// Heuristic: does a job error look like a missing model/file rather than a real bug? Such
/// tools are SKIPped (the provider simply doesn't have that resource installed).
///
/// "Value not in list" is ambiguous: a COMBO value was rejected. For a model/checkpoint
/// option that means the model isn't installed → SKIP. For a *media-input* param it means our
/// uploaded asset wasn't registered where the node could see it — a real wiring bug → FAIL.
/// We disambiguate by whether the error points at one of this tool's media inputs, so broken
/// image/video input is reported instead of hiding as a SKIP.
fn is_missing_resource(je: &JobError, desc: &ToolDescriptor) -> bool {
    let hay = format!("{} {}", je.code, je.message).to_lowercase();

    if is_value_not_in_list(&hay) && rejects_media_input(&hay, desc) {
        return false; // media input rejected → real failure, not a missing resource
    }

    const NEEDLES: &[&str] = &[
        "not in list",
        "value not in",
        "is not in",
        "not found",
        "no such file",
        "could not find",
        "not installed",
        "missing model",
        "no checkpoint",
        "unavailable",
        "does not exist",
        "no models",
        "no options",
    ];
    NEEDLES.iter().any(|n| hay.contains(n))
}

/// A rejected-COMBO ("value not in list") family error.
fn is_value_not_in_list(hay: &str) -> bool {
    ["not in list", "value not in", "is not in"]
        .iter()
        .any(|n| hay.contains(n))
}

/// Whether a "value not in list" error references one of `desc`'s media-input params (vs. a
/// model/checkpoint option). ComfyUI embeds the offending node's class (e.g. `StimmaVideoParam`)
/// and/or param name in the message; we match on the media kind in a param/picker/upload context
/// and on the schema param name. The kind word is only matched in such a context so a bare
/// "image"/"video" in an unrelated model error doesn't trigger a false FAIL.
fn rejects_media_input(hay: &str, desc: &ToolDescriptor) -> bool {
    for m in desc.media_params() {
        // The schema param name itself (e.g. "input_videos").
        if hay.contains(&m.name.to_lowercase()) {
            return true;
        }
        let kind = desc
            .property(&m.name)
            .and_then(|p| p.get("x-control"))
            .and_then(|c| c.as_str())
            .map(media_input_kind)
            .unwrap_or(Kind::Image);
        let label = kind.label();
        for needle in [
            format!("{label}param"),
            format!("{label} param"),
            format!("{label}_picker"),
            format!("{label} input"),
        ] {
            if hay.contains(&needle) {
                return true;
            }
        }
    }
    false
}

// ---- fixtures ---------------------------------------------------------------

struct Fixtures<'a> {
    args: &'a SweepArgs,
    ctx: &'a Ctx,
    client: reqwest::Client,
    cache: PathBuf,
    /// Lazily fetched manifest (None until first attempt; stays None if unreachable).
    manifest: Option<Value>,
    manifest_tried: bool,
    /// Memoized per-kind resolution.
    resolved: BTreeMap<&'static str, Option<PathBuf>>,
}

impl<'a> Fixtures<'a> {
    fn new(args: &'a SweepArgs, ctx: &'a Ctx) -> Result<Self> {
        use etcetera::BaseStrategy;
        let cache = etcetera::choose_base_strategy()
            .map(|s| s.cache_dir().join("stp").join("fixtures"))
            .unwrap_or_else(|_| std::env::temp_dir().join("stp-fixtures"));
        Ok(Fixtures {
            args,
            ctx,
            client: reqwest::Client::new(),
            cache,
            manifest: None,
            manifest_tried: false,
            resolved: BTreeMap::new(),
        })
    }

    /// A local path to a fixture for `kind`, or None if none is available (caller SKIPs).
    async fn resolve(&mut self, kind: Kind) -> Option<PathBuf> {
        if let Some(hit) = self.resolved.get(kind.label()) {
            return hit.clone();
        }
        let result = self.resolve_uncached(kind).await;
        self.resolved.insert(kind.label(), result.clone());
        result
    }

    async fn resolve_uncached(&mut self, kind: Kind) -> Option<PathBuf> {
        // 1. Explicit local override.
        let explicit = match kind {
            Kind::Image => self.args.fixture_image.as_deref(),
            Kind::Video => self.args.fixture_video.as_deref(),
            _ => None,
        };
        if let Some(p) = explicit {
            let path = PathBuf::from(p);
            if path.is_file() {
                return Some(path);
            }
            self.ctx.note(format!(
                "warning: --fixture-{} {p:?} not found",
                kind.label()
            ));
            return None;
        }

        // 2. Manifest from the assets bucket.
        let manifest = self.load_manifest().await?;
        let entry = manifest
            .get("fixtures")
            .and_then(|f| f.as_array())?
            .iter()
            .find(|e| e.get("media_kind").and_then(|k| k.as_str()) == Some(kind.label()))?;
        self.download_entry(entry).await
    }

    async fn load_manifest(&mut self) -> Option<Value> {
        if self.manifest_tried {
            return self.manifest.clone();
        }
        self.manifest_tried = true;
        let url = &self.args.assets_url;
        match self.client.get(url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.bytes().await {
                Ok(body) => match serde_json::from_slice::<Value>(&body) {
                    Ok(v) => {
                        self.manifest = Some(v);
                    }
                    Err(e) => self.ctx.note(format!("warning: bad fixture manifest: {e}")),
                },
                Err(e) => self
                    .ctx
                    .note(format!("warning: reading fixture manifest: {e}")),
            },
            Ok(resp) => self.ctx.note(format!(
                "warning: fixture manifest {url} → HTTP {}",
                resp.status()
            )),
            Err(e) => self
                .ctx
                .note(format!("warning: no fixture manifest ({url}): {e}")),
        }
        self.manifest.clone()
    }

    async fn download_entry(&self, entry: &Value) -> Option<PathBuf> {
        let key = entry.get("key").and_then(|k| k.as_str())?;
        let sha = entry.get("sha256").and_then(|s| s.as_str());
        // Keep the real extension so the uploaded asset id is e.g. `<id>.png`, not `.bin` —
        // a real provider may rely on it to decode the input.
        let basename = key.rsplit('/').next().unwrap_or(key);
        let fname = match sha {
            Some(s) => match basename.rsplit_once('.') {
                Some((_, ext)) => format!("{s}.{ext}"),
                None => s.to_string(),
            },
            None => basename.to_string(),
        };
        let path = self.cache.join(&fname);

        if path.is_file() {
            if let Some(want) = sha {
                if file_sha256(&path).as_deref() == Some(want) {
                    return Some(path);
                }
            } else {
                return Some(path);
            }
        }

        let url = self.fixture_url(entry, key);
        let bytes = match self.client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => resp.bytes().await.ok()?,
            Ok(resp) => {
                self.ctx
                    .note(format!("warning: fixture {url} → HTTP {}", resp.status()));
                return None;
            }
            Err(e) => {
                self.ctx
                    .note(format!("warning: fetching fixture {url}: {e}"));
                return None;
            }
        };
        if let Some(want) = sha {
            if bytes_sha256(&bytes) != want {
                self.ctx
                    .note(format!("warning: fixture {key} failed checksum"));
                return None;
            }
        }
        if std::fs::create_dir_all(&self.cache).is_err() {
            return None;
        }
        std::fs::write(&path, &bytes).ok()?;
        Some(path)
    }

    /// Resolve a fixture's URL: an absolute `url` field wins, else `key` relative to the
    /// manifest's directory.
    fn fixture_url(&self, entry: &Value, key: &str) -> String {
        if let Some(u) = entry.get("url").and_then(|u| u.as_str()) {
            return u.to_string();
        }
        let base = self
            .args
            .assets_url
            .rsplit_once('/')
            .map(|(b, _)| b)
            .unwrap_or(&self.args.assets_url);
        format!("{base}/{key}")
    }
}

fn file_sha256(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(bytes_sha256(&bytes))
}

fn bytes_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

// ---- overrides --------------------------------------------------------------

fn load_overrides(path: Option<&str>) -> Result<BTreeMap<String, Map<String, Value>>> {
    let Some(path) = path else {
        return Ok(BTreeMap::new());
    };
    let text = std::fs::read_to_string(path)
        .map_err(|e| StpError::usage(format!("reading overrides {path}: {e}")))?;
    // YAML is a JSON superset, so this parses either.
    let value: Value = serde_yaml_ng::from_str(&text)
        .map_err(|e| StpError::usage(format!("parsing overrides {path}: {e}")))?;
    let obj = value
        .as_object()
        .ok_or_else(|| StpError::usage("overrides must be a map of tool_id → {param: value}"))?;
    let mut out = BTreeMap::new();
    for (tool, sub) in obj {
        let sub_obj = sub.as_object().ok_or_else(|| {
            StpError::usage(format!(
                "overrides for {tool:?} must be a map of param → value"
            ))
        })?;
        out.insert(tool.clone(), sub_obj.clone());
    }
    Ok(out)
}

// ---- plan-only mode ---------------------------------------------------------

async fn plan_only(
    ctx: &Ctx,
    tools: &[ToolDescriptor],
    overrides: &BTreeMap<String, Map<String, Value>>,
    fixtures: &mut Fixtures<'_>,
) -> Result<()> {
    let _ = ctx;
    let mut tw = tabwriter::TabWriter::new(Vec::new());
    let _ = writeln!(tw, "TOOL\tTASK\tPLAN");
    for desc in tools {
        let task = desc.task_types.first().cloned().unwrap_or_default();
        let plan = build_plan(desc, overrides, fixtures).await;
        let detail = match plan {
            Plan::Skip(reason) => format!("SKIP: {reason}"),
            Plan::Run { params, .. } => {
                let summary = serde_json::to_string(&Value::Object(params)).unwrap_or_default();
                let summary = one_line(&summary);
                if summary.len() > 100 {
                    format!("{}…", &summary[..100])
                } else {
                    summary
                }
            }
        };
        let _ = writeln!(tw, "{}\t{}\t{}", desc.id, task, detail);
    }
    let bytes = tw.into_inner().unwrap_or_default();
    print!("{}", String::from_utf8_lossy(&bytes));
    Ok(())
}

// ---- reporting --------------------------------------------------------------

fn print_table(reports: &[ToolReport]) {
    let mut tw = tabwriter::TabWriter::new(Vec::new());
    let _ = writeln!(tw, "STATUS\tTOOL\tTASK\tTIME\tOUTPUT\tNOTE");
    for r in reports {
        let _ = writeln!(
            tw,
            "{}\t{}\t{}\t{}\t{}\t{}",
            r.status.word(),
            r.id,
            r.task,
            r.duration_ms.map(fmt_dur).unwrap_or_else(|| "-".into()),
            r.output.as_deref().unwrap_or("-"),
            r.note,
        );
    }
    let bytes = tw.into_inner().unwrap_or_default();
    print!("{}", String::from_utf8_lossy(&bytes));
}

fn write_json_report(path: &str, session: &Session, reports: &[ToolReport]) -> Result<()> {
    let rows: Vec<Value> = reports
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "task": r.task,
                "status": r.status.word().to_lowercase(),
                "duration_ms": r.duration_ms,
                "output": r.output,
                "note": r.note,
                "parameters": r.params,
            })
        })
        .collect();
    let doc = json!({
        "generated_at": chrono::Local::now().to_rfc3339(),
        "provider": {
            "id": session.provider.provider_id,
            "name": session.provider.provider_name,
            "server": session.provider.server,
        },
        "totals": {
            "passed": reports.iter().filter(|r| r.status == Status::Pass).count(),
            "failed": reports.iter().filter(|r| r.status == Status::Fail).count(),
            "skipped": reports.iter().filter(|r| r.status == Status::Skip).count(),
        },
        "results": rows,
    });
    std::fs::write(path, serde_json::to_string_pretty(&doc)?)
        .map_err(|e| StpError::usage(format!("writing report {path}: {e}")))?;
    Ok(())
}

fn fmt_dur(ms: u128) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

fn fmt_bytes(n: usize) -> String {
    const KB: usize = 1024;
    const MB: usize = 1024 * 1024;
    if n >= MB {
        format!("{:.1}MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1}KB", n as f64 / KB as f64)
    } else {
        format!("{n}B")
    }
}

/// Collapse whitespace/newlines so a message fits one table cell.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc(schema: Value, tasks: &[&str]) -> ToolDescriptor {
        serde_json::from_value(json!({
            "id": "t",
            "task_types": tasks,
            "parameter_schema": schema,
        }))
        .unwrap()
    }

    #[test]
    fn sniff_recognizes_formats() {
        let png = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        assert_eq!(sniff(&png), Some((Kind::Image, "png")));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF]), Some((Kind::Image, "jpg")));
        let mut mp4 = vec![0u8; 12];
        mp4[4..8].copy_from_slice(b"ftyp");
        assert_eq!(sniff(&mp4), Some((Kind::Video, "mp4")));
        let mut wav = vec![0u8; 12];
        wav[0..4].copy_from_slice(b"RIFF");
        wav[8..12].copy_from_slice(b"WAVE");
        assert_eq!(sniff(&wav), Some((Kind::Audio, "wav")));
        assert_eq!(sniff(b"not media at all"), None);
    }

    #[test]
    fn png_dimensions_parsed() {
        // 8-byte sig, then IHDR length+tag, then width=64 height=48 big-endian.
        let mut b = vec![0u8; 24];
        b[0..8].copy_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
        b[16..20].copy_from_slice(&64u32.to_be_bytes());
        b[20..24].copy_from_slice(&48u32.to_be_bytes());
        assert_eq!(dimensions("png", &b), Some((64, 48)));
    }

    #[test]
    fn expected_kind_from_tasks() {
        assert_eq!(
            expected_output_kind(&desc(json!({}), &["text-to-image"])),
            Kind::Image
        );
        assert_eq!(
            expected_output_kind(&desc(json!({}), &["image-to-video"])),
            Kind::Video
        );
        assert_eq!(
            expected_output_kind(&desc(json!({}), &["video-to-video"])),
            Kind::Video
        );
        assert_eq!(
            expected_output_kind(&desc(json!({}), &["text-to-speech"])),
            Kind::Audio
        );
    }

    #[test]
    fn media_input_kind_mapping() {
        // A frame picker takes a still image (decoded by an image node), not a video container.
        assert_eq!(media_input_kind("video_frame_picker"), Kind::Image);
        assert_eq!(media_input_kind("image_picker"), Kind::Image);
        assert_eq!(media_input_kind("mask_editor"), Kind::Image);
        // Only an explicit video picker consumes a whole video.
        assert_eq!(media_input_kind("video_picker"), Kind::Video);
    }

    #[test]
    fn missing_resource_heuristic() {
        // No media inputs on this tool → a checkpoint "not in list" is a missing model (SKIP).
        let d = desc(
            json!({"type": "object", "properties": {}}),
            &["text-to-image"],
        );
        let miss = JobError {
            code: "EXECUTION_FAILED".into(),
            message: "value 'sdxl.safetensors' not in list of available checkpoints".into(),
        };
        assert!(is_missing_resource(&miss, &d));
        let real = JobError {
            code: "EXECUTION_FAILED".into(),
            message: "tensor shape mismatch in attention block".into(),
        };
        assert!(!is_missing_resource(&real, &d));
    }

    #[test]
    fn media_input_not_in_list_is_real_failure() {
        // A tool with a required video input. ComfyUI rejects an uploaded video that didn't
        // register in StimmaVideoParam's COMBO list — that's a wiring bug, so FAIL not SKIP.
        let vid = desc(
            json!({
                "type": "object",
                "required": ["input_videos", "checkpoint"],
                "properties": {
                    "input_videos": {"type": "array", "x-control": "video_picker"},
                    "checkpoint": {"type": "string", "enum": ["a.safetensors"]}
                }
            }),
            &["upscale-video"],
        );

        // Rejected media input → real failure (the node class names the media param).
        let media_reject = JobError {
            code: "EXECUTION_FAILED".into(),
            message: "ComfyUI prompt has node validation errors: #26 (StimmaVideoParam): Value not in list".into(),
        };
        assert!(
            !is_missing_resource(&media_reject, &vid),
            "rejected video input must FAIL, not SKIP"
        );

        // A missing checkpoint on the *same* tool still SKIPs — it names a model loader, not a
        // media param.
        let ckpt_missing = JobError {
            code: "EXECUTION_FAILED".into(),
            message: "#3 (CheckpointLoaderSimple): Value 'sdxl.safetensors' not in list".into(),
        };
        assert!(
            is_missing_resource(&ckpt_missing, &vid),
            "missing checkpoint option must SKIP"
        );
    }

    #[test]
    fn cost_keys_pinned_to_minimum() {
        let prop = json!({"type": "integer", "minimum": 1, "maximum": 50, "default": 20});
        assert_eq!(min_value(&prop), Some(json!(1)));
        let enumed = json!({"type": "integer", "x-allowed-values": [16, 8, 24]});
        assert_eq!(min_value(&enumed), Some(json!(8)));
    }

    #[tokio::test]
    async fn build_plan_pins_cost_and_skips_empty_choice() {
        let ov = BTreeMap::new();
        let args = SweepArgs {
            only: vec![],
            overrides: None,
            assets_url: "http://127.0.0.1:1/x".into(),
            fixture_image: None,
            fixture_video: None,
            timeout: 1,
            out: None,
            report: None,
            list: true,
            keep_assets: false,
        };
        let ctx = Ctx {
            color: false,
            quiet: true,
        };
        let mut fx = Fixtures::new(&args, &ctx).unwrap();

        // steps pinned to min, prompt fixed, optional sampler left to default (omitted).
        let d = desc(
            json!({
                "type": "object",
                "required": ["prompt"],
                "properties": {
                    "prompt": {"type": "string"},
                    "steps": {"type": "integer", "minimum": 2, "default": 30},
                    "sampler": {"type": "string", "default": "euler"}
                }
            }),
            &["text-to-image"],
        );
        match build_plan(&d, &ov, &mut fx).await {
            Plan::Run { params, .. } => {
                assert_eq!(params.get("steps"), Some(&json!(2)));
                assert_eq!(params.get("prompt"), Some(&json!(SWEEP_PROMPT)));
                assert!(params.get("sampler").is_none(), "optional default omitted");
            }
            Plan::Skip(r) => panic!("unexpected skip: {r}"),
        }

        // A required choice with no candidates → skip (model not installed).
        let d2 = desc(
            json!({
                "type": "object",
                "required": ["checkpoint"],
                "properties": { "checkpoint": {"type": "string", "enum": []} }
            }),
            &["text-to-image"],
        );
        assert!(matches!(build_plan(&d2, &ov, &mut fx).await, Plan::Skip(_)));
    }
}
