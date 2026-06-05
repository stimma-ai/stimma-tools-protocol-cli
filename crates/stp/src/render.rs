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
pub fn show_tool(desc: &ToolDescriptor, pal: &Palette) -> String {
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
        let out = show_tool(&t2i(), &Palette::new(false));
        insta::assert_snapshot!(out);
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
        assert_eq!(default_label(&json!({"type":"string","default":"euler"})), "\"euler\"");
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
