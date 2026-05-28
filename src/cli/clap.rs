use clap::{Parser, Subcommand, ValueEnum};

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
    Status,
}

#[derive(Parser)]
pub struct Cli {
    /// Diagnostic verbosity: quiet, normal, vars, or trace.
    #[arg(long, value_enum, global = true)]
    pub verbosity: Option<CliVerbosity>,

    #[command(subcommand)]
    pub action: CliAction,
}
