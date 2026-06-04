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

/// Load and unload cascading Nix development environments.
#[derive(Parse)]
pub enum CliAction {
    /// Activate the allowed .cade environment chain for the current directory.
    Enter {
        /// Shell directive format to emit.
        #[pound(long)]
        shell: String,
    },
    /// Deactivate cade and restore the shell environment from its snapshot.
    Exit {
        /// Shell directive format to emit.
        #[pound(long)]
        shell: String,
    },
    /// Re-evaluate cade after a directory change and update the shell.
    Reload {
        /// Shell directive format to emit.
        #[pound(long)]
        shell: String,
    },
    /// Allow cade to load the current .cade directory.
    Allow,
    /// Block cade from loading the current .cade directory.
    Disallow,
    /// Open ./.cade in $EDITOR and allow this directory.
    Edit,
    /// Print shell hook initialization code.
    Hook {
        /// Shell to generate hook code for.
        shell: String,
    },
    /// Internal compatibility endpoint used by the direnv shim.
    #[pound(hidden)]
    Export { format: CliExportFormat },
    /// Manage non-shell clients that keep an environment alive.
    Lease {
        #[pound(subcommand)]
        action: LeaseAction,
    },
    /// Show activation state, layer chain, permissions, and leases.
    Status,
}

#[derive(Parse)]
pub enum LeaseAction {
    /// Open a client lease and print its client id.
    Open {
        /// Client kind recorded with the lease, such as ide or generic.
        #[pound(long, default = "generic")]
        kind: String,

        /// Project directory held by the lease. Defaults to the current directory.
        #[pound(long)]
        project: Option<PathBuf>,

        /// Lease lifetime in seconds. Defaults to the configured shell GC root TTL.
        #[pound(long)]
        ttl_seconds: Option<u64>,
    },
    /// Extend an existing client lease.
    Refresh {
        /// Client id returned by lease open.
        #[pound(long)]
        client_id: String,

        /// New lease lifetime in seconds. Defaults to the configured shell GC root TTL.
        #[pound(long)]
        ttl_seconds: Option<u64>,
    },
    /// Close an existing client lease.
    Close {
        /// Client id returned by lease open.
        #[pound(long)]
        client_id: String,
    },
}

/// Load and unload cascading Nix development environments.
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
