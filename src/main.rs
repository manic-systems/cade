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

use anyhow::{Context as _, Result};
use pound::Parse as _;
use shlex::split;
use std::env::{current_dir, current_exe, var};
use std::fs::canonicalize;
use std::path::Path;
use std::process::{Command, exit};

use crate::cli::pound::{Cli, CliAction, CliExportFormat, LeaseAction};
use crate::config::{load as load_config, set as set_config};
use crate::core::activation::export_env_delta;
use crate::core::enter::do_activation;
use crate::core::permissions::{allow_here, set_permission};
use crate::core::reload::do_reload;
use crate::core::restore::do_restore;
use crate::core::sessions::leases::{lease_close, lease_open, lease_refresh};
use crate::core::status::do_status;
use crate::core::{Announce, Cade};
use crate::shells::ShellName;
use crate::verbosity::set as set_verbosity;

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
        }
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
        }
        CliAction::Reload { shell } => {
            let cade = Cade::init()?;
            let output = ShellName::from(shell).get_output();
            do_reload(&cade, output.as_ref(), client_id.as_deref(), owner_pid)
                .context("reload cade environment")?;
        }
        CliAction::Export {
            format: CliExportFormat::Json,
        } => {
            let cade = Cade::init()?;
            let delta = export_env_delta(&cade, client_id.as_deref(), owner_pid)
                .context("export cade environment")?;
            print!("{}", delta.to_json());
        }
        CliAction::Allow => {
            allow_here(&Cade::init()?, true)?;
        }
        CliAction::Disallow => {
            allow_here(&Cade::init()?, false)?;
        }
        CliAction::Edit => {
            let cade = Cade::init()?;
            let editor = var("EDITOR").context("find EDITOR variable")?;
            let parts = split(&editor).context("parse EDITOR variable")?;
            let (program, editor_args) = parts.split_first().context("EDITOR variable is empty")?;
            let mut session = Command::new(program)
                .args(editor_args)
                .arg(".cade")
                .spawn()
                .context("spawn editor process")?;
            session.wait().context("wait for editor process")?;
            let cwd = current_dir().context("determine cwd")?;
            set_permission(&cade, &cwd, true)?;
        }
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
        }
        CliAction::Status => {
            do_status(&Cade::init()?).context("report status")?;
        }
    }
    Ok(())
}

fn main() {
    if let Err(error) = try_main() {
        eprintln!("failed to {error:#}");
        exit(1);
    }
}
