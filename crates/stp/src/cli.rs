//! clap command-line definitions.

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "stp",
    version,
    about = "Command-line host for the Stimma Tools Protocol",
    long_about = "stp is a command-line host for the Stimma Tools Protocol (STP).\n\
        Point it at one provider (cloud, remote, or a local subprocess) to list tools,\n\
        inspect schemas, and run generations.\n\n\
        Agents: run `stp agent-guide` for a machine-readable guide to driving stp.",
    propagate_version = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalOpts,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Args, Clone)]
pub struct GlobalOpts {
    /// Server alias from the config file (ssh-style).
    #[arg(short = 's', long, global = true)]
    pub server: Option<String>,

    /// One-off WebSocket provider URL (ws:// or wss://).
    #[arg(long, global = true, conflicts_with_all = ["exec", "server"])]
    pub url: Option<String>,

    /// One-off stdio provider command to spawn.
    #[arg(long, global = true, conflicts_with_all = ["url", "server"])]
    pub exec: Option<String>,

    /// Bearer token (overrides any configured token).
    #[arg(long, global = true)]
    pub token: Option<String>,

    /// Working directory for a spawned stdio provider.
    #[arg(long, global = true)]
    pub cwd: Option<String>,

    /// Extra environment for a spawned stdio provider (KEY=VALUE; repeatable).
    #[arg(long = "env", global = true, value_name = "KEY=VALUE")]
    pub env: Vec<String>,

    /// Increase verbosity (-v frames, -vv wire dumps). Diagnostics go to stderr.
    #[arg(short = 'v', long = "verbose", global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Suppress non-essential stderr output.
    #[arg(short = 'q', long, global = true)]
    pub quiet: bool,

    /// Color policy for human output.
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto)]
    pub color: ColorChoice,

    /// Override the config file path.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<String>,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

#[derive(Subcommand)]
pub enum Command {
    /// List the provider's tools.
    #[command(alias = "ls")]
    Tools(ToolsArgs),
    /// Show the provider (no tool) or a tool's parameters.
    Show(ShowArgs),
    /// Execute a tool, transfer assets, write outputs.
    Run(RunArgs),
    /// Send one arbitrary JSON-RPC request; print the response.
    Raw(RawArgs),
    /// Stream provider notifications until interrupted.
    Watch(WatchArgs),
    /// Manage the config file and server aliases.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Emit shell completions.
    Completions(CompletionsArgs),
    /// Emit a machine-readable guide to driving stp programmatically.
    AgentGuide(AgentGuideArgs),
    /// Print version and supported protocol version.
    Version,
}

#[derive(Args)]
pub struct ToolsArgs {
    /// Filter by task type.
    #[arg(long)]
    pub task: Option<String>,
    /// Emit the raw tools.list result as JSON.
    #[arg(long)]
    pub json: bool,
    /// Emit tab-separated columns (script-friendly).
    #[arg(long = "format", value_name = "FORMAT")]
    pub format: Option<String>,
}

#[derive(Args)]
pub struct ShowArgs {
    /// Tool id. Omit to show the provider's info and queue status.
    pub tool: Option<String>,
    /// Dump as JSON (the raw descriptor for a tool, or provider info with no tool).
    #[arg(long)]
    pub json: bool,
    /// Dump only the parameter_schema (with a tool).
    #[arg(long = "schema-only")]
    pub schema_only: bool,
    /// Dump only the output_schema (with a tool).
    #[arg(long = "output-schema")]
    pub output_schema: bool,
}

#[derive(Args)]
#[command(after_help = RUN_AFTER_HELP)]
pub struct RunArgs {
    /// Tool id.
    pub tool: String,
    /// Positional prompt + parameter flags (`--guidance=1.0`), intermixed with run flags.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub rest: Vec<String>,
}

/// `run` parses its own trailing argv (tool params + reserved flags intermix freely), so clap
/// can't list these — this is hand-maintained. Keep in sync with `params::parse_run`.
const RUN_AFTER_HELP: &str = "\
Parameters (run `stp show <tool>` for a tool's parameters):
  <prompt>                 Positional prompt text; `-` reads it from stdin.
  --<param> <value>        Set any tool parameter by name (repeat to build an array).
  --<flag> / --no-<flag>   Set a boolean parameter true / false.
  --set <k>=<v>            Set one parameter; `--set <k>:=<json>` for a raw JSON value.
  --params <json|@file|->  Merge a JSON object of parameters.
  --input <param>=<file>   Force-upload <file> and bind it to <param>.

Output:
  -o, --output <path>      Write to a file, or to a `dir/` (one file per output asset).
      --stdout             Stream the primary asset to stdout (binary; no -n).
      --no-download        Leave outputs on the provider; don't fetch them.
      --keep-remote        Don't delete provider-side outputs after download.
      --role <role>        Pick which output asset to write when several are returned.
      --json               Emit a JSON result (request id, assets, paths) instead of paths.

Batch & control:
  -n, --count <N>          Run N times, reusing uploaded inputs (gen_*_1, _2, …).
      --timeout <secs>     Cancel and exit 5 if the job runs longer.
      --request-id <id>    Use a specific request id instead of a generated one.

Preflight:
      --dry-run            Validate and print the request (with upload plan); don't execute.
      --no-validate        Skip schema validation.
      --allow-unknown      Pass through parameters not declared in the schema.
      --no-auto-upload     Don't auto-upload local file paths found in media params.

Examples:
  stp run flux-klein-9b \"a low-poly fox\" --steps 4
  stp run flux-klein-9b \"make it watercolor\" --input_images cat.png -o out/
  stp run flux-klein-9b \"neon city\" -n 4 -o batch/
  echo \"a cat\" | stp run flux-klein-9b - --json";

#[derive(Args)]
pub struct RawArgs {
    /// JSON-RPC method.
    pub method: String,
    /// params as inline JSON, @file, or `-` for stdin.
    pub params: Option<String>,
}

#[derive(Args)]
pub struct WatchArgs {
    /// Emit raw JSON frames (one per line) instead of formatted events.
    #[arg(long)]
    pub json: bool,
}

#[derive(Subcommand)]
pub enum ConfigCmd {
    /// List configured server aliases.
    List,
    /// Show one alias (token value is never printed).
    Get { name: String },
    /// Add or update an alias.
    Add(ConfigAddArgs),
    /// Remove an alias.
    Remove { name: String },
    /// Set the default server.
    SetDefault { name: String },
    /// Write a fully-commented template config if none exists.
    Init,
    /// Print the resolved config file path.
    Path,
    /// Open the config file in $EDITOR.
    Edit,
}

#[derive(Args)]
pub struct ConfigAddArgs {
    pub name: String,
    #[arg(long)]
    pub url: Option<String>,
    #[arg(long)]
    pub exec: Option<String>,
    #[arg(long)]
    pub cwd: Option<String>,
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub env: Vec<String>,
    #[arg(long = "token")]
    pub token: Option<String>,
    #[arg(long = "token-env")]
    pub token_env: Option<String>,
    #[arg(long = "token-command")]
    pub token_command: Option<String>,
}

#[derive(Args)]
pub struct CompletionsArgs {
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}

#[derive(Args)]
pub struct AgentGuideArgs {
    /// Emit the guide as a structured JSON manifest.
    #[arg(long)]
    pub json: bool,
}
