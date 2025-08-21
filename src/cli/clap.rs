use clap::{Parser, Subcommand};

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
    #[command(subcommand)]
    pub action: CliAction,
}
