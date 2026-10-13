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
    #[pound(alias = "0, silent, none")]
    Quiet,
    /// load, unload, and reload text and warnings
    #[pound(alias = "1, lifecycle, default")]
    Normal,
    /// normal plus the names of variables set, cleared, and restored
    #[pound(alias = "2, variables")]
    Vars,
    /// vars plus layer, cache, external command, and hook details
    #[pound(alias = "3, debug, all")]
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
    Enter {
        #[pound(long)]
        shell: CliShell,
    },

    Exit {
        #[pound(long)]
        shell: CliShell,
    },

    Reload {
        #[pound(long)]
        shell: CliShell,
    },

    Allow,

    Disallow,

    Edit,

    Hook {
        shell: CliShell,
    },

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
        #[pound(long, complete = complete_lease_id)]
        client_id: String,

        #[pound(long)]
        ttl_seconds: Option<u64>,
    },

    Close {
        #[pound(long, complete = complete_lease_id)]
        client_id: String,
    },
}

#[derive(Parse)]
pub struct Cli {
    #[pound(long)]
    pub config: Option<PathBuf>,

    #[pound(long)]
    pub verbosity: Option<CliVerbosity>,

    #[pound(long, complete = complete_lease_id)]
    pub client_id: Option<String>,

    #[pound(long)]
    pub owner_pid: Option<u32>,

    #[pound(subcommand)]
    pub action: CliAction,
}
