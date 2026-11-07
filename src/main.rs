mod cade_file;
mod cli;
mod command;
mod config;
mod core;
mod direnv_export;
mod env;
mod envrc;
mod expand;
mod loaders;
mod nix;
mod path_resolve;
mod progress;
mod shells;
mod types;
mod verbosity;

use std::{
    env::current_exe,
    fs::canonicalize,
    path::Path,
    process::exit,
};

use misstep::{
    Result,
    ResultExt as _,
};
use pound::Parse as _;

use crate::{
    cli::pound::{
        Cli,
        CliAction,
        CliExportFormat,
        LeaseAction,
    },
    config::{
        load as load_config,
        set as set_config,
    },
    core::{
        Announce,
        Cade,
        activation::export_env_delta,
        enter::do_activation,
        permissions::allow_here,
        project::edit_project,
        reload::do_reload,
        restore::do_restore,
        sessions::leases::{
            lease_close,
            lease_open,
            lease_refresh,
        },
        status::do_status,
    },
    shells::ShellName,
    verbosity::set as set_verbosity,
};

fn print_hook(shell: ShellName, config_path: Option<&Path>) -> Result<()> {
    let output = shell.get_output();
    let exe = current_exe()
        .context("resolve cade executable for shell hook")?
        .to_string_lossy()
        .into_owned();
    let hook_args = config_path
        .map(|raw_path| -> Result<Vec<String>> {
            let resolved = canonicalize(raw_path).context("resolve config path for shell hook")?;
            Ok(vec![
                "--config".to_owned(),
                resolved.to_string_lossy().into_owned(),
            ])
        })
        .transpose()?
        .unwrap_or_default();
    print!("{}", output.hook_init(&exe, &hook_args));
    Ok(())
}

fn try_main() -> Result<()> {
    let Cli {
        config,
        verbosity,
        client_id,
        owner_pid,
        action,
    } = Cli::parse();
    set_config(load_config(config.as_deref())?);
    if let Some(level) = verbosity {
        set_verbosity(level.into());
    }
    match action {
        CliAction::Hook { shell } => print_hook(shell.into(), config.as_deref())?,
        CliAction::Enter { shell } => {
            let cade = Cade::init()?;
            let output = ShellName::from(shell).get_output();
            do_activation(
                &cade,
                output.as_ref(),
                Some(Announce::Loaded),
                client_id.as_deref(),
                owner_pid,
            )
            .context("activate cade environment")?;
        },
        CliAction::Exit { shell } => {
            let cade = Cade::init()?;
            let output = ShellName::from(shell).get_output();
            do_restore(
                &cade,
                output.as_ref(),
                true,
                true,
                client_id.as_deref(),
                owner_pid,
            );
        },
        CliAction::Reload { shell } => {
            let cade = Cade::init()?;
            let output = ShellName::from(shell).get_output();
            do_reload(&cade, output.as_ref(), client_id.as_deref(), owner_pid)
                .context("reload cade environment")?;
        },
        CliAction::Export {
            format: CliExportFormat::Json,
        } => {
            let cade = Cade::init()?;
            let delta = export_env_delta(&cade, client_id.as_deref(), owner_pid)
                .context("export cade environment")?;
            print!("{}", delta.to_json());
        },
        CliAction::Allow { path } => {
            allow_here(&Cade::init()?, path.as_deref(), true)?;
        },
        CliAction::Disallow { path } => {
            allow_here(&Cade::init()?, path.as_deref(), false)?;
        },
        CliAction::Edit { path } => {
            edit_project(&Cade::init()?, path.as_deref())?;
        },
        CliAction::Lease { action: lease } => {
            let cade = Cade::init()?;
            match lease {
                LeaseAction::Open {
                    kind,
                    project,
                    ttl_seconds,
                } => lease_open(&cade, &kind, project.as_deref(), ttl_seconds)?,
                LeaseAction::Refresh {
                    client_id: lease_client,
                    ttl_seconds,
                } => lease_refresh(&cade, &lease_client, ttl_seconds)?,
                LeaseAction::Close {
                    client_id: lease_client,
                } => lease_close(&cade, &lease_client)?,
            }
        },
        CliAction::Status => {
            do_status(&Cade::init()?).context("report status")?;
        },
    }
    Ok(())
}

fn main() {
    caress::handle(Cli::SPEC);
    if let Err(error) = try_main() {
        eprintln!("failed to {error}");
        exit(1);
    }
}
