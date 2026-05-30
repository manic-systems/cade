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

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliExportFormat {
    Json,
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
    /// Internal compatibility endpoint used by the direnv shim.
    #[command(hide = true)]
    Export {
        #[arg(value_enum)]
        format: CliExportFormat,
    },
    Status,
}

#[derive(Parser)]
pub struct Cli {
    /// Strictly read this TOML config file instead of the XDG default.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    /// Diagnostic verbosity: quiet, normal, vars, or trace.
    #[arg(long, value_enum, global = true)]
    pub verbosity: Option<CliVerbosity>,

    #[command(subcommand)]
    pub action: CliAction,
}
