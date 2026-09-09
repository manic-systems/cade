use crate::command::run_checked_output;
use crate::env::set::EnvSet;
use crate::nix::capture;
use crate::nix::profile::wipe_history;
use crate::nix::target::FlakeTarget;
use crate::types::layer::NixDevEnv;
use anyhow::{Context as _, Result};
use std::collections::BTreeMap;
use std::env::{split_paths, var_os, vars};
use std::fs::{canonicalize, create_dir_all};
use std::io::{Write as _, stderr};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn load_flake(target: &FlakeTarget, profile: &Path) -> Result<(NixDevEnv, EnvSet)> {
    let mut proc = Command::new("nix");
    proc.arg("develop");
    if !target.installable.is_empty() {
        proc.arg(&target.installable);
    }
    load_nix_dev_env(proc, &target.cwd, profile)
}

pub fn load_shell(file: &Path, profile: &Path) -> Result<(NixDevEnv, EnvSet)> {
    let cwd = file.parent().unwrap_or(file);
    let mut proc = Command::new("nix");
    proc.args(["develop", "-f"]).arg(file);
    load_nix_dev_env(proc, cwd, profile)
}

fn load_nix_dev_env(mut proc: Command, path: &Path, profile: &Path) -> Result<(NixDevEnv, EnvSet)> {
    if let Some(parent) = profile.parent() {
        create_dir_all(parent)
            .with_context(|| format!("creating nix profile dir at {}", parent.display()))?;
    }
    proc.arg("--profile").arg(profile);
    let mut env = capture_dev_env(proc, path)?;
    let store_path = canonicalize(profile)
        .with_context(|| format!("resolving nix environment at {}", profile.display()))?;
    env.retain_store_path(store_path.to_string_lossy().into_owned());
    wipe_history(profile);
    Ok((
        NixDevEnv {
            store_path,
            cwd: path.to_path_buf(),
        },
        env,
    ))
}

impl NixDevEnv {
    pub fn activate(&self) -> Result<EnvSet> {
        let mut proc = Command::new("nix");
        proc.arg("develop").arg(&self.store_path);
        let mut env = capture_dev_env(proc, &self.cwd)?;
        env.retain_store_path(self.store_path.to_string_lossy().into_owned());
        Ok(env)
    }
}

fn capture_dev_env(mut proc: Command, cwd: &Path) -> Result<EnvSet> {
    let mut previous_env: BTreeMap<_, _> = vars().collect();
    proc.args(["--log-format", "internal-json", "--command"])
        .arg(find_on_path("sh"))
        .args(["-c", capture::env_capture_script(), "cade-env"])
        .arg(find_on_path("env"))
        .current_dir(cwd);
    capture::remove_cade_managed_env(&mut previous_env, &mut proc);

    let output = run_checked_output(proc, &format!("nix develop at {}", cwd.display()))?;
    let (hook_stdout, raw_env) =
        capture::captured_env_output(&output.stdout, &format!("at {}", cwd.display()))?;
    let mut error_stream = stderr().lock();
    error_stream
        .write_all(hook_stdout)
        .context("write nix shellHook output")?;
    for line in output.stderr.split_inclusive(|byte| *byte == b'\n') {
        if !line.starts_with(b"@nix ") {
            error_stream
                .write_all(line)
                .context("write nix shellHook error output")?;
        }
    }
    capture::env_set_from_captured_env(raw_env, &previous_env)
}

fn find_on_path(name: &str) -> PathBuf {
    var_os("PATH")
        .and_then(|path| {
            split_paths(&path)
                .map(|dir| dir.join(name))
                .find(|candidate| {
                    candidate.metadata().is_ok_and(|metadata| {
                        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                    })
                })
        })
        .unwrap_or_else(|| PathBuf::from(name))
}
