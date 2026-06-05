//! The `agent-guide` content: a machine door so agents drive stp without scraping the human UI.

use serde_json::json;

pub const MARKDOWN: &str = r#"# Driving `stp` programmatically

`stp` is a host for one Stimma Tools Protocol provider per invocation. Prefer JSON in and JSON
out; never parse the human tables or progress bars.

## Selecting a provider
- `-s <alias>`  use a configured server (see `stp config list`)
- `--url wss://…/stp-v1 [--token T]`  one-off WebSocket
- `--exec '<cmd>'`  spawn a stdio provider

## Canonical workflow
1. `stp agent-guide --json`                     learn this interface (once)
2. `stp -s S tools --json`                       discover tools (raw tools.list result)
3. `stp -s S show <tool> --schema-only`        get the parameter JSON Schema
4. build a parameters object; preview without spending a job:
   `echo "$PARAMS" | stp -s S run <tool> --params - --dry-run --json`
5. execute and consume the result:
   `echo "$PARAMS" | stp -s S run <tool> --params - --json --no-download`

## The robust parameter path
Pass a whole object with `--params @file` or `--params -` (stdin) instead of assembling
`--<param>` flags — it avoids flag-name collisions and shell quoting. Flags still work and
override the blob. Arrays/objects in flags are JSON (`--loras='[…]'` or `--set k:=<json>`).

## Output
- default: assets download to a fresh `./stp-out-<request_id>/`; paths printed on stdout
- `--no-download`: write nothing (metadata only; pair with `--json`)
- `--stdout`: one selected asset's bytes to stdout (buffered)
- `-o DIR/` or `-o FILE`; `--role R` selects among multi-asset outputs
- `--json`: full tools.result (output.assets[] + metadata) on stdout

## Exit codes (branch on these, not stderr text)
0 ok · 1 runtime · 2 usage · 3 connection/protocol · 4 job failed · 5 timeout ·
6 validation · 130 interrupted

## Result shape (`run --json`)
{ "request_id": "...", "success": true,
  "output": { "assets": [ {"asset_id":"…","type":"image","role":"primary"} ] },
  "metadata": { "generation_time": 1.0 },
  "paths": ["/abs/path/to/asset.png"] }
"#;

pub fn manifest() -> serde_json::Value {
    json!({
        "tool": "stp",
        "role": "host",
        "protocol": "Stimma Tools Protocol 1.0",
        "provider_selection": {
            "alias": "-s <name>",
            "websocket": "--url wss://…/stp-v1 [--token T]",
            "stdio": "--exec '<cmd>'",
            "env": ["STP_SERVER", "STP_URL", "STP_TOKEN", "STP_EXEC"]
        },
        "workflow": [
            "stp agent-guide --json",
            "stp -s S tools --json",
            "stp -s S show <tool> --schema-only",
            "stp -s S run <tool> --params - --dry-run --json",
            "stp -s S run <tool> --params - --json --no-download"
        ],
        "parameters": {
            "preferred": "--params @file | --params - (full JSON object)",
            "flags": "--<param>=<value>; arrays/objects are JSON; --set k:=<json> escape",
            "validation": "schema-validated before execute; --dry-run previews; --no-validate skips"
        },
        "output": {
            "default": "download to ./stp-out-<request_id>/, paths on stdout",
            "no_download": "--no-download (metadata only)",
            "stdout": "--stdout (single asset bytes)",
            "select": "--role <r> | -o FILE for single-asset"
        },
        "exit_codes": {
            "0": "success", "1": "runtime", "2": "usage",
            "3": "connection or protocol (JSON-RPC error)", "4": "job failed",
            "5": "timeout", "6": "validation", "130": "interrupted"
        },
        "result_shape": {
            "request_id": "string",
            "success": "bool",
            "output": { "assets": [{"asset_id": "string", "type": "string", "role": "string"}] },
            "metadata": "object",
            "paths": ["string (when downloaded)"]
        }
    })
}
