use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliVerbosity {
    Quiet,
    Normal,
    Vars,
    Trace,
}

impl From<CliVerbosity> for crate::verbosity::Verbosity {
    fn from(value: CliVerbosity) -> Self {
        match value {
            CliVerbosity::Quiet => Self::Quiet,
            CliVerbosity::Normal => Self::Normal,
            CliVerbosity::Vars => Self::Vars,
            CliVerbosity::Trace => Self::Trace,
        }
    }
}

#[derive(Subcommand)]
pub enum CliAction {
    Enter {
        #[arg(long)]
        shell: String,
    },
    Exit {
        #[arg(long)]
        shell: String,
    },
    Reload {
        #[arg(long)]
        shell: String,
    },
    Allow,
    Disallow,
    Edit,
    Hook {
        shell: String,
    },
    Lease {
        #[command(subcommand)]
        action: LeaseAction,
    },
    Status,
}

#[derive(Subcommand)]
pub enum LeaseAction {
    Open {
        #[arg(long, default_value = "generic")]
        kind: String,

        #[arg(long)]
        project: Option<PathBuf>,

        #[arg(long)]
        ttl_seconds: Option<u64>,
    },
    Refresh {
        #[arg(long)]
        client_id: String,

        #[arg(long)]
        ttl_seconds: Option<u64>,
    },
    Close {
        #[arg(long)]
        client_id: String,
    },
}

#[derive(Parser)]
pub struct Cli {
    /// Strictly read this TOML config file instead of the XDG default.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    /// Diagnostic verbosity: quiet, normal, vars, or trace.
    #[arg(long, value_enum, global = true)]
    pub verbosity: Option<CliVerbosity>,

    /// Lease client id to refresh while activating or reloading.
    #[arg(long, global = true)]
    pub client_id: Option<String>,

    /// Process id that owns this cade environment; defaults to cade's parent.
    #[arg(long, global = true)]
    pub owner_pid: Option<u32>,

    #[command(subcommand)]
    pub action: CliAction,
}
