//! `stp` — command-line host for the Stimma Tools Protocol.
//!
//! `main` stays thin: parse args, short-circuit no-connection commands, otherwise build a
//! current-thread runtime and dispatch. Every failure carries a typed exit code (SPEC §4).

mod agent_guide;
mod cli;
mod commands;
mod config;
mod connect;
mod ctx;
mod params;
mod render;
mod sweep;

use clap::Parser;
use cli::{Cli, Command};
use ctx::Ctx;
use stp_client::error::{ExitCode, StpError};

fn main() {
    let cli = Cli::parse();
    ctx::init_tracing(cli.global.verbose, cli.global.quiet);

    let code = match run(cli) {
        Ok(()) => ExitCode::Success.code(),
        Err(e) => {
            match &e {
                StpError::Interrupted => eprintln!("interrupted"),
                _ => eprintln!("stp: {e}"),
            }
            e.exit_code().code()
        }
    };
    std::process::exit(code);
}

fn run(cli: Cli) -> stp_client::error::Result<()> {
    let ctx = Ctx::new(&cli.global);
    match cli.command {
        Command::Version => {
            commands::version();
            Ok(())
        }
        Command::Completions(a) => commands::completions(a),
        Command::AgentGuide(a) => commands::agent_guide(a),
        Command::Config(c) => commands::config_cmd(&cli.global, c),
        connected => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(StpError::from)?;
            rt.block_on(commands::run_connected(&cli.global, &ctx, connected))
        }
    }
}
