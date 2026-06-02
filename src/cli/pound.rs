use pound::{Parse, ValueEnum};
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

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliExportFormat {
    Json,
}

#[derive(Parse)]
pub enum CliAction {
    Enter {
        #[pound(long)]
        shell: String,
    },
    Exit {
        #[pound(long)]
        shell: String,
    },
    Reload {
        #[pound(long)]
        shell: String,
    },
    Allow,
    Disallow,
    Edit,
    Hook {
        shell: String,
    },
    /// Internal compatibility endpoint used by the direnv shim.
    #[pound(hidden)]
    Export {
        format: CliExportFormat,
    },
    Lease {
        #[pound(subcommand)]
        action: LeaseAction,
    },
    Status,
}

#[derive(Parse)]
pub enum LeaseAction {
    Open {
        #[pound(long, default = "generic")]
        kind: String,

        #[pound(long)]
        project: Option<PathBuf>,

        #[pound(long)]
        ttl_seconds: Option<u64>,
    },
    Refresh {
        #[pound(long)]
        client_id: String,

        #[pound(long)]
        ttl_seconds: Option<u64>,
    },
    Close {
        #[pound(long)]
        client_id: String,
    },
}

#[derive(Parse)]
pub struct Cli {
    /// Strictly read this TOML config file instead of the XDG default.
    #[pound(long)]
    pub config: Option<PathBuf>,

    /// Diagnostic verbosity: quiet, normal, vars, or trace.
    #[pound(long)]
    pub verbosity: Option<CliVerbosity>,

    /// Lease client id to attach while activating or reloading.
    #[pound(long)]
    pub client_id: Option<String>,

    /// Shell process pid to hold this activation's GC roots.
    #[pound(long)]
    pub owner_pid: Option<u32>,

    #[pound(subcommand)]
    pub action: CliAction,
}
