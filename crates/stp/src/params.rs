//! Parse `run`'s trailing argv into a parameters object + run options.
//!
//! stp's reserved run flags and dynamic per-parameter flags intermix freely. We pull out the
//! reserved flags (known names + arities); every other `--name`/`--name=value` is a tool
//! parameter, coerced against the schema. The first bare token is the positional prompt
//! (`-` = stdin). `--` ends reserved-flag parsing.

use serde_json::{Map, Value};
use std::io::Read;
use stp_client::descriptor::ToolDescriptor;
use stp_client::error::{Result, StpError};
use stp_client::schema;

#[derive(Debug, Clone)]
pub enum OutputMode {
    /// Default: a fresh per-run subdir.
    DefaultDir,
    Dir(String),
    File(String),
    Stdout,
    None,
}

pub struct ParsedRun {
    pub params: Map<String, Value>,
    /// Explicit `--input param=file` bindings (always uploaded).
    pub forced_inputs: Vec<(String, String)>,
    pub output: OutputMode,
    pub role: Option<String>,
    pub timeout: Option<u64>,
    pub request_id: Option<String>,
    pub count: u32,
    pub json: bool,
    pub dry_run: bool,
    pub no_validate: bool,
    pub allow_unknown: bool,
    pub no_auto_upload: bool,
    pub keep_remote: bool,
}

fn prop_type<'a>(desc: &'a ToolDescriptor, name: &str) -> Option<&'a str> {
    desc.property(name)
        .and_then(|p| p.get("type"))
        .and_then(|t| t.as_str())
}

fn validation_err(msg: impl std::fmt::Display) -> StpError {
    StpError::Validation(format!("  {msg}"))
}

pub fn parse_run(desc: &ToolDescriptor, rest: &[String]) -> Result<ParsedRun> {
    let mut base: Option<Map<String, Value>> = None; // from --params (merged first)
    let mut prompt: Option<Value> = None;
    let mut flags: Vec<(String, Value)> = Vec::new(); // ordered scalar/literal overrides
    let mut array_accum: Vec<(String, Value)> = Vec::new(); // repeated array elements
    let mut forced_inputs = Vec::new();

    let mut output: Option<OutputMode> = None;
    let mut no_download = false;
    let mut stdout = false;
    let mut role = None;
    let mut timeout = None;
    let mut request_id = None;
    let mut count: u32 = 1;
    let mut json = false;
    let mut dry_run = false;
    let mut no_validate = false;
    let mut allow_unknown = false;
    let mut no_auto_upload = false;
    let mut keep_remote = false;

    let mut only_params = false;
    let mut i = 0;
    while i < rest.len() {
        let tok = &rest[i];
        i += 1;

        if !only_params && tok == "--" {
            only_params = true;
            continue;
        }

        // bare positional → prompt
        if tok == "-" || !tok.starts_with('-') {
            if prompt.is_some() {
                return Err(StpError::usage(format!(
                    "unexpected positional argument {tok:?} (only the prompt is positional)"
                )));
            }
            let text = if tok == "-" {
                let mut s = String::new();
                std::io::stdin()
                    .read_to_string(&mut s)
                    .map_err(StpError::from)?;
                s.trim_end_matches('\n').to_string()
            } else {
                tok.clone()
            };
            prompt = Some(Value::String(text));
            continue;
        }

        // short -o / -n
        if tok == "-o" {
            let v = take_value(rest, &mut i, "-o")?;
            output = Some(classify_output(&v));
            continue;
        }
        if tok == "-n" {
            let v = take_value(rest, &mut i, "-n")?;
            count = parse_count(&v)?;
            continue;
        }

        // long flag
        let body = tok.trim_start_matches('-');
        let (name, inline) = match body.split_once('=') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (body.to_string(), None),
        };

        if !only_params {
            match name.as_str() {
                "output" => {
                    let v = inline_or_next(inline, rest, &mut i, "--output")?;
                    output = Some(classify_output(&v));
                    continue;
                }
                "role" => {
                    role = Some(inline_or_next(inline, rest, &mut i, "--role")?);
                    continue;
                }
                "timeout" => {
                    let v = inline_or_next(inline, rest, &mut i, "--timeout")?;
                    timeout = Some(v.parse().map_err(|_| {
                        StpError::usage(format!("--timeout expects seconds, got {v:?}"))
                    })?);
                    continue;
                }
                "request-id" => {
                    request_id = Some(inline_or_next(inline, rest, &mut i, "--request-id")?);
                    continue;
                }
                "count" => {
                    let v = inline_or_next(inline, rest, &mut i, "--count")?;
                    count = parse_count(&v)?;
                    continue;
                }
                "input" => {
                    let v = inline_or_next(inline, rest, &mut i, "--input")?;
                    let (p, f) = v
                        .split_once('=')
                        .ok_or_else(|| StpError::usage("--input expects param=file"))?;
                    forced_inputs.push((p.to_string(), f.to_string()));
                    continue;
                }
                "params" => {
                    let v = inline_or_next(inline, rest, &mut i, "--params")?;
                    base = Some(load_params_blob(&v)?);
                    continue;
                }
                "set" => {
                    let v = inline_or_next(inline, rest, &mut i, "--set")?;
                    let (k, val) = parse_set(desc, &v)?;
                    flags.push((k, val));
                    continue;
                }
                "stdout" => {
                    stdout = true;
                    continue;
                }
                "json" => {
                    json = true;
                    continue;
                }
                "no-download" => {
                    no_download = true;
                    continue;
                }
                "dry-run" => {
                    dry_run = true;
                    continue;
                }
                "no-validate" => {
                    no_validate = true;
                    continue;
                }
                "allow-unknown" => {
                    allow_unknown = true;
                    continue;
                }
                "no-auto-upload" => {
                    no_auto_upload = true;
                    continue;
                }
                "keep-remote" => {
                    keep_remote = true;
                    continue;
                }
                _ => {}
            }
        }

        // --no-<bool-param> → false
        if let Some(stripped) = name.strip_prefix("no-") {
            if prop_type(desc, stripped) == Some("boolean") && inline.is_none() {
                flags.push((stripped.to_string(), Value::Bool(false)));
                continue;
            }
        }

        // dynamic tool parameter
        let raw = match inline {
            Some(v) => v,
            None => {
                if prop_type(desc, &name) == Some("boolean") {
                    flags.push((name, Value::Bool(true)));
                    continue;
                }
                take_value(rest, &mut i, &format!("--{name}"))?
            }
        };
        // Array params: a `[`/`{`-leading value is JSON; a bare value is a single element,
        // so repeated `--param x --param y` builds the array.
        if prop_type(desc, &name) == Some("array") && !raw.trim_start().starts_with('[') {
            array_accum.push((name, Value::String(raw)));
            continue;
        }
        let coerced = schema::coerce(desc.property(&name), &raw)
            .map_err(|e| validation_err(format!("{name}: {e}")))?;
        flags.push((name, coerced));
    }

    // assemble: blob → prompt → flags
    let mut params = base.unwrap_or_default();
    if let Some(p) = prompt {
        if desc.property("prompt").is_none() {
            return Err(StpError::usage(
                "this tool has no `prompt` parameter; pass values as --<param> flags",
            ));
        }
        params.insert("prompt".to_string(), p);
    }
    for (k, v) in flags {
        params.insert(k, v);
    }
    for (k, v) in array_accum {
        match params.get_mut(&k) {
            Some(Value::Array(a)) => a.push(v),
            _ => {
                params.insert(k, Value::Array(vec![v]));
            }
        }
    }

    // output precedence: --stdout > --no-download > -o/--output > default
    let output = if stdout {
        OutputMode::Stdout
    } else if no_download {
        OutputMode::None
    } else {
        output.unwrap_or(OutputMode::DefaultDir)
    };

    Ok(ParsedRun {
        params,
        forced_inputs,
        output,
        role,
        timeout,
        request_id,
        count,
        json,
        dry_run,
        no_validate,
        allow_unknown,
        no_auto_upload,
        keep_remote,
    })
}

fn parse_count(v: &str) -> Result<u32> {
    match v.trim().parse::<u32>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(StpError::usage(format!(
            "-n/--count expects a positive integer, got {v:?}"
        ))),
    }
}

fn classify_output(v: &str) -> OutputMode {
    if v.ends_with('/') || std::path::Path::new(v).is_dir() {
        OutputMode::Dir(v.to_string())
    } else {
        OutputMode::File(v.to_string())
    }
}

fn take_value(rest: &[String], i: &mut usize, flag: &str) -> Result<String> {
    if *i < rest.len() {
        let v = rest[*i].clone();
        *i += 1;
        Ok(v)
    } else {
        Err(StpError::usage(format!("{flag} expects a value")))
    }
}

fn inline_or_next(
    inline: Option<String>,
    rest: &[String],
    i: &mut usize,
    flag: &str,
) -> Result<String> {
    match inline {
        Some(v) => Ok(v),
        None => take_value(rest, i, flag),
    }
}

fn parse_set(desc: &ToolDescriptor, spec: &str) -> Result<(String, Value)> {
    if let Some((k, v)) = spec.split_once(":=") {
        let val = schema::parse_json_value(v).map_err(|e| validation_err(format!("{k}: {e}")))?;
        return Ok((k.to_string(), val));
    }
    let (k, v) = spec
        .split_once('=')
        .ok_or_else(|| StpError::usage("--set expects key=value or key:=json"))?;
    let val =
        schema::coerce(desc.property(k), v).map_err(|e| validation_err(format!("{k}: {e}")))?;
    Ok((k.to_string(), val))
}

fn load_params_blob(arg: &str) -> Result<Map<String, Value>> {
    let text = if arg == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(StpError::from)?;
        s
    } else if let Some(path) = arg.strip_prefix('@') {
        std::fs::read_to_string(path).map_err(StpError::from)?
    } else {
        arg.to_string()
    };
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| StpError::usage(format!("--params is not valid JSON: {e}")))?;
    match v {
        Value::Object(m) => Ok(m),
        _ => Err(StpError::usage("--params must be a JSON object")),
    }
}
