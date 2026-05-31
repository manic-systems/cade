mod cli;
mod config;
mod core;
mod envrc;
mod envs;
mod loaders;
mod shells;
mod types;
mod verbosity;

use anyhow::{Context, Result};
use clap::Parser;

use crate::core::{Announce, Cade};
use crate::shells::ShellName;

fn try_main() -> Result<()> {
    let args = cli::clap::Cli::parse();
    let config = crate::config::load(args.config.as_deref())?;
    crate::config::set(config);
    if let Some(verbosity) = args.verbosity {
        crate::verbosity::set(verbosity.into());
    }
    use cli::clap::CliAction::*;

    // `hook` emits a static snippet, so handle it before the side-effecting init
    if let Hook { shell } = &args.action {
        let shell_name: ShellName = shell.parse().map_err(|e: String| anyhow::anyhow!(e))?;
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
            let shell_name: ShellName = shell.parse().map_err(|e: String| anyhow::anyhow!(e))?;
            let output = shell_name.get_output();
            cade.do_activation(
                output.as_ref(),
                Announce::Loaded,
                args.client_id.as_deref(),
                args.owner_pid,
            )
            .context("activate cade environment")?;
        }
        Exit { shell } => {
            let shell_name: ShellName = shell.parse().map_err(|e: String| anyhow::anyhow!(e))?;
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
            let shell_name: ShellName = shell.parse().map_err(|e: String| anyhow::anyhow!(e))?;
            let output = shell_name.get_output();
            cade.do_reload(output.as_ref(), args.client_id.as_deref(), args.owner_pid)
                .context("reload cade environment")?;
        }
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
            // edit targets ./.cade, so allow the cwd
            let cwd = std::env::current_dir().context("determine cwd")?;
            cade.set_permission(&cwd, true)?;
        }
        Hook { .. } => unreachable!("handled before Cade::init()"),
        Lease { action } => {
            use cli::clap::LeaseAction::*;
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
        eprintln!("failed to {e}\n{}", e.root_cause());
        std::process::exit(1);
    }
}
