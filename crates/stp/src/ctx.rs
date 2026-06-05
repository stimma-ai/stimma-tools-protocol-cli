//! Output context: TTY/color resolution and diagnostics setup.

use crate::cli::{ColorChoice, GlobalOpts};
use std::io::IsTerminal;

pub struct Ctx {
    pub color: bool,
    pub quiet: bool,
}

impl Ctx {
    pub fn new(g: &GlobalOpts) -> Ctx {
        let color = match g.color {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => {
                std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
            }
        };
        Ctx {
            color,
            quiet: g.quiet,
        }
    }

    /// A note to stderr (suppressed under --quiet).
    pub fn note(&self, msg: impl std::fmt::Display) {
        if !self.quiet {
            eprintln!("{msg}");
        }
    }
}

/// Initialise tracing to stderr; level scales with -v. Protocol frames + provider stderr
/// surface at debug/trace.
pub fn init_tracing(verbose: u8, quiet: bool) {
    use tracing_subscriber::EnvFilter;
    let level = if quiet {
        "error"
    } else {
        match verbose {
            0 => "warn",
            1 => "debug",
            _ => "trace",
        }
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(format!("stp={level},stp_client={level},provider={level}"))
    });
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_env_filter(filter)
        .without_time()
        .try_init();
}
