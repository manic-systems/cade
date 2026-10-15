use std::path::PathBuf;

use pound::{
    Parse,
    ValueEnum,
    complete::{
        Candidate,
        Partial,
    },
};

use crate::{
    core::sessions::leases::lease_ids,
    shells::ShellName,
    verbosity::Verbosity,
};

fn complete_lease_id(_partial: &Partial<'_, '_>, _typed: &str) -> Vec<Candidate> {
    lease_ids().into_iter().map(Candidate::new).collect()
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[pound(ignore_case)]
pub enum CliVerbosity {
    /// no lifecycle text or warnings
    #[pound(alias = ["0", "silent", "none"])]
    Quiet,
    /// load, unload, and reload text and warnings
    #[pound(alias = ["1", "lifecycle", "default"])]
    Normal,
    /// normal plus the names of variables set, cleared, and restored
    #[pound(alias = ["2", "variables"])]
    Vars,
    /// vars plus layer, cache, external command, and hook details
    #[pound(alias = ["3", "debug", "all"])]
    Trace,
}

impl From<CliVerbosity> for Verbosity {
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
#[pound(ignore_case)]
pub enum CliShell {
    Fish,
    Bash,
    Zsh,
    #[pound(alias = "nu")]
    Nushell,
    Json,
    Elvish,
    Murex,
}

impl From<CliShell> for ShellName {
    fn from(value: CliShell) -> Self {
        match value {
            CliShell::Fish => Self::Fish,
            CliShell::Bash => Self::Bash,
            CliShell::Zsh => Self::Zsh,
            CliShell::Nushell => Self::Nushell,
            CliShell::Json => Self::Json,
            CliShell::Elvish => Self::Elvish,
            CliShell::Murex => Self::Murex,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliExportFormat {
    Json,
}

#[derive(Parse)]
pub enum CliAction {
    /// activate the environment for the current directory, run by the hook
    Enter {
        /// the shell to print activation code for
        #[pound(long)]
        shell: CliShell,
    },

    /// deactivate and restore the previous environment
    Exit {
        /// the shell to print restore code for
        #[pound(long)]
        shell: CliShell,
    },

    /// re-evaluate after a directory change, run by the hook
    Reload {
        /// the shell to print reload code for
        #[pound(long)]
        shell: CliShell,
    },

    /// allow cade in the current .cade directory
    Allow,

    /// block cade in the current .cade directory
    Disallow,

    /// open .cade in $EDITOR, then allow this directory
    Edit,

    /// print the shell hook initialization code
    #[pound(after_help = "Examples:\n  eval \"$(cade hook bash)\"\n  cade hook fish | source")]
    Hook {
        /// the shell to print the hook for
        shell: CliShell,
    },

    #[pound(hidden)]
    Export { format: CliExportFormat },

    /// manage leases that keep a non-shell client's GC roots alive
    Lease {
        #[pound(subcommand)]
        action: LeaseAction,
    },

    /// show activation state, the layer chain, and permissions
    Status,
}

#[derive(Parse)]
pub enum LeaseAction {
    /// open a lease and print it as JSON
    Open {
        /// what kind of client holds the lease, such as ide
        #[pound(long, default = "generic")]
        kind: String,

        /// the project directory the lease is for
        #[pound(long)]
        project: Option<PathBuf>,

        /// how long the lease lasts, the GC root retention by default
        #[pound(long, min = 1)]
        ttl_seconds: Option<u64>,
    },

    /// extend a lease
    Refresh {
        /// the lease to extend
        #[pound(long, complete = complete_lease_id)]
        client_id: String,

        /// how long the lease lasts from now, the GC root retention by default
        #[pound(long, min = 1)]
        ttl_seconds: Option<u64>,
    },

    /// close a lease and release its GC roots
    Close {
        /// the lease to close
        #[pound(long, complete = complete_lease_id)]
        client_id: String,
    },
}

/// an intelligent, cascading environment manager
#[derive(Parse)]
#[pound(help_if_empty)]
pub struct Cli {
    /// read this config file instead of the XDG default, failing if it is
    /// missing
    #[pound(long)]
    pub config: Option<PathBuf>,

    /// how much cade reports
    #[pound(long)]
    pub verbosity: Option<CliVerbosity>,

    /// the lease to attach this session to
    #[pound(long, complete = complete_lease_id)]
    pub client_id: Option<String>,

    /// the process that owns this session, the parent process by default
    #[pound(long, min = 1)]
    pub owner_pid: Option<u32>,

    #[pound(subcommand)]
    pub action: CliAction,
}
