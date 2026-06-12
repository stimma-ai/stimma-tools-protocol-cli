//! Human-facing renderers. Pure `String` builders so they snapshot-test cleanly.
//!
//! Color is structural and restrained: dim for labels/secondary text, one cyan accent for
//! identifiers, a yellow marker for required fields, bold for titles. Alignment is computed on
//! *visible* width (escape codes excluded), so colored cells still line up.

use std::fmt::Write as _;
use stp_client::descriptor::ToolDescriptor;
use stp_client::session::Session;
use tabwriter::TabWriter;

/// A restrained ANSI palette, gated on whether color is enabled.
#[derive(Clone, Copy)]
pub struct Palette {
    pub color: bool,
}

impl Palette {
    pub fn new(color: bool) -> Self {
        Palette { color }
    }
    fn wrap(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
    pub fn dim(&self, s: &str) -> String {
        self.wrap("2", s)
    }
    pub fn bold(&self, s: &str) -> String {
        self.wrap("1", s)
    }
    pub fn accent(&self, s: &str) -> String {
        self.wrap("36", s)
    }
    pub fn warn(&self, s: &str) -> String {
        self.wrap("33", s)
    }
}

/// Public elastic-tab formatter (used by `config list`, which is uncolored).
pub fn tabify_public(rows: &str) -> String {
    tabify(rows)
}

fn tabify(rows: &str) -> String {
    use std::io::Write;
    let mut tw = TabWriter::new(Vec::new()).padding(2);
    let _ = tw.write_all(rows.as_bytes());
    let _ = tw.flush();
    String::from_utf8(tw.into_inner().unwrap_or_default()).unwrap_or_default()
}

/// Visible terminal width of a plain (escape-free) string. Uses Unicode display width so CJK
/// and other wide glyphs — common in negative-prompt defaults — pad and align correctly.
fn disp(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    s.width()
}

/// One table cell: the plain text (for width) and its styled form (for display).
type Cell = (String, String);

fn cell(plain: impl Into<String>, styled: String) -> Cell {
    (plain.into(), styled)
}

/// Render rows under headers with 2-space gutters, padding by visible width so colored cells
/// stay aligned. The last column isn't padded.
fn columns(headers: &[&str], rows: &[Vec<Cell>], pal: &Palette) -> String {
    let n = headers.len();
    let mut w = vec![0usize; n];
    for (i, h) in headers.iter().enumerate() {
        w[i] = disp(h);
    }
    for r in rows {
        for (i, (plain, _)) in r.iter().enumerate() {
            if i < n {
                w[i] = w[i].max(disp(plain));
            }
        }
    }
    let header_cells: Vec<Cell> = headers.iter().map(|h| cell(*h, pal.dim(h))).collect();
    let mut out = String::new();
    out.push_str(&render_row(&header_cells, &w));
    for r in rows {
        out.push('\n');
        out.push_str(&render_row(r, &w));
    }
    out
}

fn render_row(cells: &[Cell], w: &[usize]) -> String {
    let n = cells.len();
    let mut s = String::new();
    for (i, (plain, styled)) in cells.iter().enumerate() {
        s.push_str(styled);
        if i + 1 < n {
            let pad = w[i].saturating_sub(disp(plain)) + 2;
            s.push_str(&" ".repeat(pad));
        }
    }
    s.trim_end().to_string()
}

/// `tools` — aligned table; ids accented, task types dimmed.
pub fn tools_table(tools: &[ToolDescriptor], pal: &Palette) -> String {
    let rows: Vec<Vec<Cell>> = tools
        .iter()
        .map(|t| {
            let tt = t.task_types.join(", ");
            vec![
                cell(&t.id, pal.accent(&t.id)),
                cell(t.display_name(), t.display_name().to_string()),
                cell(&tt, pal.dim(&tt)),
            ]
        })
        .collect();
    columns(&["ID", "NAME", "TASK TYPES"], &rows, pal)
}

/// `tools --format=tsv` — one tab between columns, no padding or color.
pub fn tools_tsv(tools: &[ToolDescriptor]) -> String {
    let mut out = String::new();
    for t in tools {
        let _ = writeln!(
            out,
            "{}\t{}\t{}",
            t.id,
            t.display_name(),
            t.task_types.join(",")
        );
    }
    out.trim_end().to_string()
}

fn required_set(desc: &ToolDescriptor) -> Vec<String> {
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

/// A concise one-line note: the description plus the single most useful constraint in
/// parentheses. Long enums collapse to a count rather than dumping every value. No UI hints
/// (`x-control` and friends are Stimma-UI concerns, not useful at a CLI).
fn param_notes(schema: &serde_json::Value) -> String {
    let desc = schema
        .get("description")
        .and_then(|d| d.as_str())
        .filter(|d| !d.is_empty());
    let tag = constraint_tag(schema);
    match (desc, tag) {
        (Some(d), Some(t)) => format!("{d} ({t})"),
        (Some(d), None) => d.to_string(),
        (None, Some(t)) => t,
        (None, None) => String::new(),
    }
}

/// The most useful single constraint: numeric range, allowed values (short enums) or count
/// (long ones), or array item bounds.
fn constraint_tag(schema: &serde_json::Value) -> Option<String> {
    if let (Some(mn), Some(mx)) = (schema.get("minimum"), schema.get("maximum")) {
        return Some(format!("{mn}\u{2013}{mx}"));
    }
    if let Some(en) = schema.get("enum").and_then(|e| e.as_array()) {
        let vals: Vec<String> = en
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        if vals.len() <= 4 && !vals.is_empty() {
            return Some(vals.join(", "));
        }
        return Some(format!("{} options", en.len()));
    }
    let lo = schema
        .get("minItems")
        .or_else(|| schema.get("x-min-items"))
        .and_then(|v| v.as_u64());
    let hi = schema
        .get("maxItems")
        .or_else(|| schema.get("x-max-items"))
        .and_then(|v| v.as_u64());
    if lo.is_some() || hi.is_some() {
        return Some(items_tag(lo, hi));
    }
    None
}

/// A parameter's `enum` members as display strings — string members verbatim, anything else
/// (numeric/boolean enums) as its compact JSON form. `None` when the schema declares no `enum`.
fn enum_values(schema: &serde_json::Value) -> Option<Vec<String>> {
    schema.get("enum").and_then(|e| e.as_array()).map(|arr| {
        arr.iter()
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect()
    })
}

/// Item-count constraint, phrased the way a person reads it: a fixed count is `exactly N`, an
/// open bound is `≥N` / `≤N`, and the noun agrees in number (`1 item`, `2 items`).
fn items_tag(lo: Option<u64>, hi: Option<u64>) -> String {
    fn noun(n: u64) -> &'static str {
        if n == 1 {
            "item"
        } else {
            "items"
        }
    }
    match (lo, hi) {
        (Some(a), Some(b)) if a == b => format!("exactly {a} {}", noun(a)),
        (Some(a), Some(b)) => format!("{a}\u{2013}{b} {}", noun(b)),
        (Some(a), None) => format!("\u{2265}{a} {}", noun(a)),
        (None, Some(b)) => format!("\u{2264}{b} {}", noun(b)),
        (None, None) => String::new(),
    }
}

fn type_label(schema: &serde_json::Value) -> String {
    let ty = schema.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match ty {
        "array" => {
            let item = schema
                .pointer("/items/type")
                .and_then(|t| t.as_str())
                .unwrap_or("string");
            format!("{item}[]")
        }
        "" => "any".to_string(),
        other => other.to_string(),
    }
}

/// Max display width for the DEFAULT column. Real tools ship paragraph-length prompt defaults
/// and long CJK negative prompts; without a cap the column is sized to the longest value and
/// pushes every other row's note hundreds of columns to the right. The full value is still
/// available via `show <tool> --schema-only` / `--json`.
const MAX_DEFAULT_WIDTH: usize = 28;

fn default_label(schema: &serde_json::Value) -> String {
    if schema.get("x-control").and_then(|c| c.as_str()) == Some("seed") {
        return "random".to_string();
    }
    match schema.get("default") {
        None => "\u{2014}".to_string(),
        Some(serde_json::Value::String(s)) if s.is_empty() => "\"\"".to_string(),
        Some(serde_json::Value::String(s)) => format!("\"{}\"", elide(s, MAX_DEFAULT_WIDTH)),
        Some(other) => elide(&other.to_string(), MAX_DEFAULT_WIDTH),
    }
}

/// A single-line, width-capped form: collapse whitespace to spaces, then truncate to `max`
/// display columns with an ellipsis when shortened. Budgeting on display width (not char count)
/// keeps a CJK default — twice as wide per glyph — from blowing out the column. Keeps one
/// long/multi-line default from breaking the table row.
fn elide(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut out = String::new();
    let mut width = 0usize;
    let mut truncated = false;
    for c in s.chars() {
        let c = if c.is_whitespace() { ' ' } else { c };
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if width + w > max {
            truncated = true;
            break;
        }
        out.push(c);
        width += w;
    }
    if truncated {
        format!("{}\u{2026}", out.trim_end())
    } else {
        out
    }
}

/// `show <tool>` — a narrow aligned name/type/default table, with a dim detail line under each
/// parameter so the columns stay tight even for tools with long enums or controlnet lists.
/// With `expand_values`, a trailing section lists every enum parameter's full value set (what
/// `--values` is for); otherwise long enums stay collapsed to a count in the note column.
pub fn show_tool(desc: &ToolDescriptor, pal: &Palette, expand_values: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}   {}",
        pal.bold(desc.display_name()),
        pal.dim(&format!("({})", desc.id))
    );
    if !desc.task_types.is_empty() {
        let _ = writeln!(out, "{}", pal.dim(&desc.task_types.join(", ")));
    }
    let badges = desc.badges();
    if !badges.is_empty() {
        let _ = writeln!(out, "{}", pal.dim(&badges.join(" \u{b7} ")));
    }
    out.push('\n');

    let required = required_set(desc);
    // Media params accept a local file path or a URL; first-time users have no other cue, so
    // we tack that hint onto their note.
    let media: std::collections::HashSet<String> =
        desc.media_params().into_iter().map(|m| m.name).collect();
    // One row per parameter: name · type · default · note. The note column is last, so a long
    // description just runs to the right instead of breaking the table.
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    if let Some(props) = desc
        .parameter_schema
        .get("properties")
        .and_then(|p| p.as_object())
    {
        for (name, schema) in props {
            let req = required.contains(name);
            let name_plain = if req {
                format!("{name} *")
            } else {
                name.clone()
            };
            let name_styled = if req {
                format!("{}{}", pal.accent(name), pal.warn(" *"))
            } else {
                pal.accent(name)
            };
            let ty = type_label(schema);
            let default = default_label(schema);
            let mut note = param_notes(schema);
            if media.contains(name) {
                let hint = "file path or URL";
                note = if note.is_empty() {
                    hint.to_string()
                } else {
                    format!("{note} \u{b7} {hint}")
                };
            }
            rows.push(vec![
                cell(name_plain, name_styled),
                cell(&ty, pal.dim(&ty)),
                cell(&default, pal.dim(&default)),
                cell(&note, pal.dim(&note)),
            ]);
        }
    }
    let _ = writeln!(
        out,
        "{}",
        columns(&["PARAMETER", "TYPE", "DEFAULT", ""], &rows, pal)
    );

    // `--values`: the note column collapses long enums to `(N options)`; here we spell every
    // one out in full, in an aligned trailing section that mirrors `Layout` / `Output roles`.
    if expand_values {
        let enums: Vec<(&String, Vec<String>)> = desc
            .parameter_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .into_iter()
            .flatten()
            .filter_map(|(name, schema)| {
                enum_values(schema)
                    .filter(|v| !v.is_empty())
                    .map(|v| (name, v))
            })
            .collect();
        if !enums.is_empty() {
            let _ = writeln!(out, "\n{}", pal.dim("Allowed values"));
            let w = enums.iter().map(|(n, _)| disp(n)).max().unwrap_or(0);
            for (name, vals) in &enums {
                let pad = " ".repeat(w.saturating_sub(disp(name)) + 2);
                let _ = writeln!(out, "  {}{}{}", pal.accent(name), pad, vals.join(", "));
            }
        }
    }

    if !desc.layout.is_empty() {
        let _ = writeln!(out, "\n{}", pal.dim("Layout"));
        for sec in &desc.layout {
            let names: Vec<String> = sec.params.iter().map(|p| p.name.clone()).collect();
            let collapsed = if sec.collapsed { " (collapsed)" } else { "" };
            let _ = writeln!(out, "  {}{}: {}", sec.label, collapsed, names.join(", "));
        }
    }

    let roles = desc.output_roles();
    let _ = write!(out, "\n{} {}", pal.dim("Output roles:"), roles.join(", "));
    out
}

/// Step through any array layers to the schema that carries `properties` — the object itself, or
/// the element type of an `object[]`. Returns the schema unchanged when it has no `items`.
fn item_schema(schema: &serde_json::Value) -> &serde_json::Value {
    let mut cur = schema;
    while cur.get("properties").is_none() {
        match cur.get("items") {
            Some(items) => cur = items,
            None => break,
        }
    }
    cur
}

/// The field names directly under a container schema (an object, or an `object[]`'s items).
fn field_names(schema: &serde_json::Value) -> Vec<String> {
    item_schema(schema)
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// Whether a schema is something we can drill into — an object or an array of objects.
fn is_container(schema: &serde_json::Value) -> bool {
    item_schema(schema).get("properties").is_some()
}

/// A container's `required` field names (descending into `object[]` items).
fn required_of(schema: &serde_json::Value) -> Vec<String> {
    item_schema(schema)
        .get("required")
        .and_then(|r| r.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Resolve a dotted parameter path (`loras` or `loras.name`) against a tool's `parameter_schema`,
/// descending through object `properties` and array `items`. `None` if any segment is absent.
pub fn resolve_param<'a>(
    parameter_schema: &'a serde_json::Value,
    path: &str,
) -> Option<&'a serde_json::Value> {
    let mut cur = parameter_schema;
    for seg in path.split('.') {
        cur = item_schema(cur).get("properties")?.get(seg)?;
    }
    Some(cur)
}

/// The field names available at the level a (possibly failed) dotted path points into, so a
/// "no such parameter" error can suggest what *is* there.
pub fn param_candidates(parameter_schema: &serde_json::Value, path: &str) -> Vec<String> {
    let parent = match path.rfind('.') {
        Some(i) => resolve_param(parameter_schema, &path[..i]),
        None => Some(parameter_schema),
    };
    parent
        .map(field_names)
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| field_names(parameter_schema))
}

/// `show <tool> <param>` — one parameter in full. For a scalar it spells out every allowed value,
/// the numeric range, item bounds, and media/ControlNet hints the table elides. For an object or
/// `object[]` it lists the item's fields, each drillable in turn via a dotted `<param>.<field>`
/// path. `None` if the tool has no such parameter (the caller turns that into a usage error).
pub fn show_param(desc: &ToolDescriptor, path: &str, pal: &Palette) -> Option<String> {
    let schema = resolve_param(&desc.parameter_schema, path)?;

    // `required`/media are relative to the field's immediate parent container.
    let (parent_path, leaf) = match path.rfind('.') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    };
    let parent = if parent_path.is_empty() {
        &desc.parameter_schema
    } else {
        resolve_param(&desc.parameter_schema, parent_path)?
    };
    let required = required_of(parent).iter().any(|r| r == leaf);
    let is_media = !path.contains('.') && desc.media_params().into_iter().any(|m| m.name == path);

    // Header: path · type · required/optional · default (when one is declared).
    let mut bits = vec![
        pal.accent(path),
        pal.dim(&type_label(schema)),
        pal.dim(if required { "required" } else { "optional" }),
    ];
    let has_default = schema.get("default").is_some()
        || schema.get("x-control").and_then(|c| c.as_str()) == Some("seed");
    if has_default {
        bits.push(pal.dim(&format!("default {}", default_label(schema))));
    }
    let mut out = String::new();
    let _ = writeln!(out, "{}", bits.join(&pal.dim(" \u{b7} ")));

    if let Some(d) = schema
        .get("description")
        .and_then(|d| d.as_str())
        .filter(|d| !d.is_empty())
    {
        let _ = writeln!(out, "\n{d}");
    }

    // An object / object[] parameter: list the item's fields. Each is itself drillable, so the
    // enums nested under them (e.g. a 2000-entry `loras.name`) are one `<param>.<field>` away.
    if is_container(schema) {
        let item = item_schema(schema);
        let req = required_of(schema);
        let mut rows: Vec<Vec<Cell>> = Vec::new();
        if let Some(props) = item.get("properties").and_then(|p| p.as_object()) {
            for (fname, fschema) in props {
                let r = req.iter().any(|x| x == fname);
                let name_plain = if r {
                    format!("{fname} *")
                } else {
                    fname.clone()
                };
                let name_styled = if r {
                    format!("{}{}", pal.accent(fname), pal.warn(" *"))
                } else {
                    pal.accent(fname)
                };
                let ty = type_label(fschema);
                let def = default_label(fschema);
                let note = param_notes(fschema);
                rows.push(vec![
                    cell(name_plain, name_styled),
                    cell(&ty, pal.dim(&ty)),
                    cell(&def, pal.dim(&def)),
                    cell(&note, pal.dim(&note)),
                ]);
            }
        }
        let _ = writeln!(out, "\n{}", pal.dim("Fields"));
        let _ = writeln!(
            out,
            "{}",
            columns(&["FIELD", "TYPE", "DEFAULT", ""], &rows, pal)
        );
        let _ = write!(
            out,
            "{}",
            pal.dim(&format!(
                "\nSee a field's values: stp show {} {}.<field>",
                desc.id, path
            ))
        );
        return Some(out.trim_end().to_string());
    }

    // A scalar parameter: spell out the constraints the table can only summarize.
    let mut cons = String::new();
    if let Some(vals) = enum_values(schema) {
        let default = schema.get("default").and_then(|d| d.as_str());
        let _ = writeln!(
            cons,
            "{}",
            pal.dim(&format!("Allowed values ({}):", vals.len()))
        );
        for v in &vals {
            let tail = if Some(v.as_str()) == default {
                format!("  {}", pal.dim("(default)"))
            } else {
                String::new()
            };
            let _ = writeln!(cons, "  {} {}{}", pal.dim("\u{2022}"), v, tail);
        }
    }
    if let (Some(mn), Some(mx)) = (schema.get("minimum"), schema.get("maximum")) {
        match schema.get("x-step").or_else(|| schema.get("multipleOf")) {
            Some(step) => {
                let _ = writeln!(cons, "Range: {mn}\u{2013}{mx} (step {step})");
            }
            None => {
                let _ = writeln!(cons, "Range: {mn}\u{2013}{mx}");
            }
        }
    }
    let lo = schema
        .get("minItems")
        .or_else(|| schema.get("x-min-items"))
        .and_then(|v| v.as_u64());
    let hi = schema
        .get("maxItems")
        .or_else(|| schema.get("x-max-items"))
        .and_then(|v| v.as_u64());
    if lo.is_some() || hi.is_some() {
        let _ = writeln!(cons, "{}", items_tag(lo, hi));
    }
    if let Some(cn) = schema.get("x-controlnet").and_then(|c| c.as_array()) {
        let names: Vec<&str> = cn.iter().filter_map(|v| v.as_str()).collect();
        if !names.is_empty() {
            let _ = writeln!(cons, "ControlNet hints: {}", names.join(", "));
        }
    }
    if is_media {
        let _ = writeln!(cons, "Accepts: file path or URL");
    }
    if !cons.is_empty() {
        let _ = writeln!(out, "\n{}", cons.trim_end());
    }
    Some(out.trim_end().to_string())
}

/// `show` (no tool) — provider snapshot. Keys dimmed, the provider name bold.
pub fn show_server(
    session: &Session,
    tool_count: usize,
    queue: Option<(u32, u32, u32)>,
    pal: &Palette,
) -> String {
    let p = &session.provider;
    let mut rows: Vec<(&str, String)> = vec![
        (
            "provider",
            pal.bold(p.provider_name.as_deref().unwrap_or(&p.provider_id)),
        ),
        ("id", p.provider_id.clone()),
    ];
    if let Some(s) = &p.server {
        rows.push(("server", s.clone()));
    }
    rows.push((
        "stp version",
        p.stp_version.clone().unwrap_or_else(|| "—".into()),
    ));
    rows.push(("transport", pal.accent(&session.transport)));
    if let Some(ep) = &session.asset_endpoint {
        rows.push(("assets", ep.clone()));
    }
    rows.push(("max concurrent", p.max_concurrent.to_string()));
    rows.push((
        "capabilities",
        if p.capabilities.cancel {
            "cancel".into()
        } else {
            "—".into()
        },
    ));
    rows.push(("tools", tool_count.to_string()));
    if let Some((q, r, c)) = queue {
        rows.push(("queue", format!("{q} queued, {r} running, capacity {c}")));
    }

    let key_w = rows.iter().map(|(k, _)| disp(k)).max().unwrap_or(0);
    let mut out = String::new();
    for (k, v) in &rows {
        let _ = writeln!(
            out,
            "{}{}{}",
            pal.dim(k),
            " ".repeat(key_w.saturating_sub(disp(k)) + 2),
            v
        );
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn t2i() -> ToolDescriptor {
        serde_json::from_value(json!({
            "id": "flux-klein-9b",
            "name": "Flux.2 Klein 9B",
            "task_types": ["text-to-image", "image-to-image"],
            "metadata": {"badges": ["Open Weights", "Fast"]},
            "parameter_schema": {
                "type": "object", "required": ["prompt"],
                "properties": {
                    "prompt": {"type": "string", "x-control": "prompt_editor", "default": "",
                               "description": "Describe the image"},
                    "width": {"type": "integer", "minimum": 256, "maximum": 2048,
                              "x-step": 64, "x-control": "resolution", "default": 1024},
                    "seed": {"type": "integer", "x-control": "seed"},
                    "input_images": {"type": "array", "items": {"type": "string"},
                        "x-control": "image_picker", "x-min-items": 0, "x-max-items": 10,
                        "x-controlnet": ["canny", "depth", "lineart", "pose"]},
                    "sampler": {"type": "string", "default": "euler", "x-control": "dropdown",
                        "enum": ["euler", "heun", "dpmpp_2m", "ddim", "lcm"]}
                }
            },
            "output_schema": {"type": "object", "properties": {"assets": {"type": "array"}}},
            "layout": [{"label": "Advanced", "collapsed": true,
                        "params": [{"name": "seed"}, {"name": "sampler"}]}]
        }))
        .unwrap()
    }

    /// A tool whose `loras` parameter is an `object[]` with a nested enum — the shape that
    /// motivated dotted-path drill-down (the allowed values live under `loras.name`).
    fn lora_tool() -> ToolDescriptor {
        serde_json::from_value(json!({
            "id": "flux-klein-9b",
            "name": "Flux.2 Klein 9B",
            "task_types": ["text-to-image"],
            "parameter_schema": {
                "type": "object", "required": ["prompt"],
                "properties": {
                    "prompt": {"type": "string", "description": "Describe the image"},
                    "loras": {"type": "array", "default": [], "description": "LoRAs to apply",
                        "items": {"type": "object", "required": ["path"], "properties": {
                            "name": {"type": "string",
                                     "enum": ["bigeyes", "watercolor", "lowpoly"]},
                            "path": {"type": "string",
                                     "enum": ["loras/bigeyes.safetensors",
                                              "loras/watercolor.safetensors",
                                              "loras/lowpoly.safetensors"]},
                            "weight": {"type": "number", "minimum": -2.0, "maximum": 2.0,
                                       "default": 1.0}
                        }}}
                }
            },
            "output_schema": {"type": "object", "properties": {"assets": {"type": "array"}}}
        }))
        .unwrap()
    }

    #[test]
    fn snapshot_tools_table() {
        let edit: ToolDescriptor = serde_json::from_value(json!({
            "id": "sdxl-upscale", "name": "SDXL Upscaler",
            "task_types": ["upscale-image"], "parameter_schema": {}, "output_schema": {}
        }))
        .unwrap();
        let out = tools_table(&[t2i(), edit], &Palette::new(false));
        insta::assert_snapshot!(out);
    }

    #[test]
    fn snapshot_show_tool() {
        let out = show_tool(&t2i(), &Palette::new(false), false);
        insta::assert_snapshot!(out);
    }

    #[test]
    fn snapshot_show_tool_values() {
        // `--values` spells out the sampler enum the note column would otherwise collapse.
        let out = show_tool(&t2i(), &Palette::new(false), true);
        insta::assert_snapshot!(out);
    }

    #[test]
    fn snapshot_show_param_enum() {
        let out = show_param(&t2i(), "sampler", &Palette::new(false)).unwrap();
        insta::assert_snapshot!(out);
    }

    #[test]
    fn snapshot_show_param_numeric() {
        let out = show_param(&t2i(), "width", &Palette::new(false)).unwrap();
        insta::assert_snapshot!(out);
    }

    #[test]
    fn show_param_unknown_is_none() {
        assert!(show_param(&t2i(), "nope", &Palette::new(false)).is_none());
    }

    #[test]
    fn snapshot_show_param_object_array() {
        // `show <tool> loras` lists the item fields and points at the dotted drill-down.
        let out = show_param(&lora_tool(), "loras", &Palette::new(false)).unwrap();
        insta::assert_snapshot!(out);
    }

    #[test]
    fn snapshot_show_param_nested_enum() {
        // `show <tool> loras.name` reaches the enum nested under the array's item object.
        let out = show_param(&lora_tool(), "loras.name", &Palette::new(false)).unwrap();
        insta::assert_snapshot!(out);
    }

    #[test]
    fn resolve_param_walks_dotted_paths() {
        let t = lora_tool();
        let s = &t.parameter_schema;
        assert!(resolve_param(s, "loras").is_some());
        assert_eq!(
            resolve_param(s, "loras.weight")
                .and_then(|w| w.get("maximum"))
                .and_then(|m| m.as_f64()),
            Some(2.0)
        );
        // `weight` is a scalar — can't descend further, and a bogus field is absent.
        assert!(resolve_param(s, "loras.weight.nope").is_none());
        assert!(resolve_param(s, "loras.bogus").is_none());
    }

    #[test]
    fn param_candidates_suggests_sibling_fields() {
        let t = lora_tool();
        let mut got = param_candidates(&t.parameter_schema, "loras.bogus");
        got.sort();
        assert_eq!(got, vec!["name", "path", "weight"]);
        // A bad top-level name falls back to the tool's parameters.
        let mut top = param_candidates(&t.parameter_schema, "nope");
        top.sort();
        assert_eq!(top, vec!["loras", "prompt"]);
    }

    #[test]
    fn long_default_is_elided() {
        let para = "Urban alleyway at dusk. Tall, statuesque high-fashion model striding.";
        let schema = json!({"type": "string", "default": para});
        let label = default_label(&schema);
        // Quoted, with the ellipsis inside the closing quote.
        assert!(label.starts_with('"') && label.ends_with('"'), "{label}");
        assert!(label.contains('\u{2026}'), "{label}");
        // At most MAX_DEFAULT_WIDTH content chars + ellipsis + two quotes.
        assert!(label.chars().count() <= MAX_DEFAULT_WIDTH + 3, "{label}");
        // Short defaults are untouched.
        assert_eq!(
            default_label(&json!({"type":"string","default":"euler"})),
            "\"euler\""
        );
    }

    #[test]
    fn item_counts_read_naturally() {
        assert_eq!(items_tag(Some(1), Some(1)), "exactly 1 item");
        assert_eq!(items_tag(Some(2), Some(2)), "exactly 2 items");
        assert_eq!(items_tag(Some(0), Some(10)), "0\u{2013}10 items");
        assert_eq!(items_tag(Some(1), None), "\u{2265}1 item");
        assert_eq!(items_tag(None, Some(1)), "\u{2264}1 item");
    }
}
