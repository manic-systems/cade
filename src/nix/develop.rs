use std::{
    collections::{
        BTreeMap,
        BTreeSet,
    },
    env::{
        split_paths,
        var_os,
        vars,
    },
    fs::{
        canonicalize,
        create_dir_all,
        read,
    },
    io::{
        Write as _,
        stderr,
    },
    os::unix::fs::PermissionsExt as _,
    path::{
        Path,
        PathBuf,
    },
    process::Command,
};

use misstep::{
    Result,
    ResultExt as _,
};
use serde::Deserialize;

use crate::{
    command::run_checked_output,
    env::set::EnvSet,
    nix::{
        capture,
        profile::wipe_history,
        target::FlakeTarget,
    },
    types::layer::NixDevEnv,
};

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
    let (env, store_path, has_shell_hook) = capture_dev_env(proc, path, || {
        canonicalize(profile)
            .with_context(|| format!("resolving nix environment at {}", profile.display()))
            .erase()
    })?;
    wipe_history(profile);
    let replay = (!has_shell_hook).then(|| env.clone());
    Ok((
        NixDevEnv {
            store_path,
            cwd: path.to_path_buf(),
            replay,
        },
        env,
    ))
}

#[derive(Deserialize)]
struct DevEnvFile {
    variables: BTreeMap<String, DevEnvVariable>,
}

#[derive(Deserialize)]
struct DevEnvVariable {
    #[serde(rename = "type")]
    kind:  DevEnvKind,
    #[serde(default)]
    value: serde_json::Value,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum DevEnvKind {
    Exported,
    Var,
    Array,
    Associative,
    Unknown,
}

impl DevEnvFile {
    fn read(store_path: &Path) -> Option<Self> {
        serde_json::from_slice(&read(store_path).ok()?).ok()
    }

    fn exported(&self) -> BTreeSet<String> {
        self.variables
            .iter()
            .filter(|entry| entry.1.kind == DevEnvKind::Exported)
            .map(|entry| entry.0.clone())
            .collect()
    }

    fn has_shell_hook(&self) -> bool {
        self.variables
            .get("shellHook")
            .and_then(|hook| hook.value.as_str())
            .is_some_and(|hook| !hook.trim().is_empty())
    }
}

impl NixDevEnv {
    pub fn activate(&self) -> Result<EnvSet> {
        if let Some(replay) = self.replay.as_ref() {
            return Ok(replay.clone());
        }
        let mut proc = Command::new("nix");
        proc.arg("develop").arg(&self.store_path);
        let (env, ..) = capture_dev_env(proc, &self.cwd, || Ok(self.store_path.clone()))?;
        Ok(env)
    }
}

fn capture_dev_env(
    mut proc: Command,
    cwd: &Path,
    resolve_store_path: impl FnOnce() -> Result<PathBuf>,
) -> Result<(EnvSet, PathBuf, bool)> {
    let mut previous_env = vars().collect::<BTreeMap<String, String>>();
    // NOTE: We need to use `-u` over `--unset-env-var` here since mainline
    // Nix and Lix use different flags. Both accept the short form `-u`,
    // but while Nix's long-form for unsetting env vars is `--unset-env-var`,
    // Lix's is `--unset`.
    proc.args(["-u", "LD_PRELOAD"])
        .args(["--log-format", "internal-json", "--command"])
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

    let store_path = resolve_store_path()?;
    let dev_env = DevEnvFile::read(&store_path);
    let exported = dev_env
        .as_ref()
        .map(DevEnvFile::exported)
        .unwrap_or_default();
    let has_shell_hook = dev_env.as_ref().is_none_or(DevEnvFile::has_shell_hook);
    let mut env = capture::env_set_from_captured_env(raw_env, &previous_env, &exported)?;
    env.root_by_store_path(store_path.to_string_lossy().into_owned());
    Ok((env, store_path, has_shell_hook))
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
