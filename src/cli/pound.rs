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

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum CliShell {
    Fish,
    Bash,
    Zsh,
    Nushell,
    Nu,
    Json,
    Elvish,
    Murex,
}

impl From<CliShell> for crate::shells::ShellName {
    fn from(value: CliShell) -> Self {
        match value {
            CliShell::Fish => Self::Fish,
            CliShell::Bash => Self::Bash,
            CliShell::Zsh => Self::Zsh,
            CliShell::Nushell | CliShell::Nu => Self::Nushell,
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
    /// Deactivate cade and restore the shell environment from its snapshot.
    Exit {
        #[pound(long)]
        shell: CliShell,
    },
    /// Re-evaluate cade after a directory change and update the shell.
    Reload {
        #[pound(long)]
        shell: CliShell,
    },
    /// Allow cade to load the current .cade directory.
    Allow,
    /// Block cade from loading the current .cade directory.
    Disallow,
    /// Open ./.cade in $EDITOR and allow this directory.
    Edit,
    /// Print shell hook initialization code.
    Hook {
        shell: CliShell,
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
    /// Show activation state, layer chain, permissions, and leases.
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
    /// Extend an existing client lease.
    Refresh {
        #[pound(long)]
        client_id: String,

        #[pound(long)]
        ttl_seconds: Option<u64>,
    },
    /// Close an existing client lease.
    Close {
        #[pound(long)]
        client_id: String,
    },
}

#[derive(Parse)]
pub struct Cli {
    #[pound(long)]
    pub config: Option<PathBuf>,

    #[pound(long)]
    pub verbosity: Option<CliVerbosity>,

    #[pound(long)]
    pub client_id: Option<String>,

    #[pound(long)]
    pub owner_pid: Option<u32>,

    #[pound(subcommand)]
    pub action: CliAction,
}

#[cfg(test)]
mod tests {
    use pound::Parse;

    use super::{Cli, CliAction, CliShell};

    #[test]
    fn shell_switches_use_typed_values() {
        let cli = Cli::try_parse_from(["enter", "--shell", "nu"]).expect("parse shell value");
        let CliAction::Enter { shell } = cli.action else {
            panic!("expected enter action");
        };

        assert_eq!(shell, CliShell::Nu);
        assert!(matches!(
            crate::shells::ShellName::from(shell),
            crate::shells::ShellName::Nushell
        ));
    }
}
