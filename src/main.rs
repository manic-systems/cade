mod cade_file;
mod cli;
mod config;
mod core;
mod direnv_export;
mod env;
mod env_delta;
mod envrc;
mod expand;
mod loaders;
mod nix_dev_env;
mod nix_progress;
mod path_resolve;
mod progress;
mod shells;
mod types;
mod verbosity;

use anyhow::{Context, Result};
use pound::Parse;

use crate::core::{Announce, Cade};

fn try_main() -> Result<()> {
    let args = cli::pound::Cli::parse();
    let config = crate::config::load(args.config.as_deref())?;
    crate::config::set(config);
    if let Some(verbosity) = args.verbosity {
        crate::verbosity::set(verbosity.into());
    }
    use cli::pound::CliAction::*;

    // static hook path
    if let Hook { shell } = &args.action {
        let shell_name: crate::shells::ShellName = (*shell).into();
        let output = shell_name.get_output();
        let cade_exe = std::env::current_exe()
            .context("resolve cade executable for shell hook")?
            .to_string_lossy()
            .into_owned();
        let cade_args = args
            .config
            .as_ref()
            .map(|path| -> Result<Vec<String>> {
                let path =
                    std::fs::canonicalize(path).context("resolve config path for shell hook")?;
                Ok(vec![
                    "--config".to_string(),
                    path.to_string_lossy().into_owned(),
                ])
            })
            .transpose()?
            .unwrap_or_default();
        print!("{}", output.hook_init(&cade_exe, &cade_args));
        return Ok(());
    }

    let mut cade = Cade::init()?;
    match args.action {
        Enter { shell } => {
            let shell_name: crate::shells::ShellName = shell.into();
            let output = shell_name.get_output();
            cade.do_activation(
                output.as_ref(),
                Some(Announce::Loaded),
                args.client_id.as_deref(),
                args.owner_pid,
            )
            .context("activate cade environment")?;
        }
        Exit { shell } => {
            let shell_name: crate::shells::ShellName = shell.into();
            let output = shell_name.get_output();
            cade.do_restore(
                output.as_ref(),
                true,
                true,
                args.client_id.as_deref(),
                args.owner_pid,
            )
            .context("deactivate cade environment")?;
        }
        Reload { shell } => {
            let shell_name: crate::shells::ShellName = shell.into();
            let output = shell_name.get_output();
            cade.do_reload(output.as_ref(), args.client_id.as_deref(), args.owner_pid)
                .context("reload cade environment")?;
        }
        Export { format } => match format {
            cli::pound::CliExportFormat::Json => {
                let delta = cade
                    .export_env_delta(args.client_id.as_deref(), args.owner_pid)
                    .context("export cade environment")?;
                print!("{}", delta.to_json());
            }
        },
        Allow => cade.allow_here(true)?,
        Disallow => cade.allow_here(false)?,
        Edit => {
            let editor = std::env::var("EDITOR").context("find EDITOR variable")?;
            let parts = shlex::split(&editor).context("parse EDITOR variable")?;
            let (program, args) = parts.split_first().context("EDITOR variable is empty")?;
            let mut session = std::process::Command::new(program)
                .args(args)
                .arg(".cade")
                .spawn()
                .context("spawn editor process")?;
            session.wait().context("wait for editor process")?;
            // edit targets ./.cade
            let cwd = std::env::current_dir().context("determine cwd")?;
            cade.set_permission(&cwd, true)?;
        }
        Hook { .. } => unreachable!("handled before Cade::init()"),
        Lease { action } => {
            use cli::pound::LeaseAction::*;
            match action {
                Open {
                    kind,
                    project,
                    ttl_seconds,
                } => cade.lease_open(&kind, project.as_deref(), ttl_seconds)?,
                Refresh {
                    client_id,
                    ttl_seconds,
                } => cade.lease_refresh(&client_id, ttl_seconds)?,
                Close { client_id } => cade.lease_close(&client_id)?,
            }
        }
        Status => cade.do_status().context("report status")?,
    };
    Ok(())
}

fn main() {
    if let Err(e) = try_main() {
        // keep the context chain
        eprintln!("failed to {e:#}");
        std::process::exit(1);
    }
}
