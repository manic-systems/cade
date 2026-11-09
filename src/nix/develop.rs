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
    process::{
        Command,
        Stdio,
    },
    sync::OnceLock,
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
    types::layer::{
        NixDevEnv,
        ShellDrv,
    },
};

pub fn load_flake(target: &FlakeTarget, profile: &Path) -> Result<(NixDevEnv, EnvSet)> {
    let mut proc = Command::new("nix");
    proc.arg("develop");
    if !target.installable.is_empty() {
        proc.arg(&target.installable);
    }
    load_nix_dev_env(proc, &target.cwd, profile)
}

// Must run before nix develop. A drvPath newer than the captured env would
// pass revalidation and keep replaying the stale env.
pub fn flake_shell_drv(target: &FlakeTarget, known: &[ShellDrv]) -> Option<ShellDrv> {
    if let Some(drv) = known
        .iter()
        .find(|drv| drv.installable == target.installable)
    {
        return Some(drv.clone());
    }
    // Read-only evals can't fetch inputs missing from flake.lock
    resolve_shell_drv(&target.cwd, &target.installable, true)
        .or_else(|| resolve_shell_drv(&target.cwd, &target.installable, false))
}

fn resolve_shell_drv(cwd: &Path, installable: &str, read_only: bool) -> Option<ShellDrv> {
    let (flake, fragment) = installable.split_once('#').unwrap_or((installable, ""));
    if fragment.contains('"') {
        return None;
    }
    let flake_ref = if flake.is_empty() { "." } else { flake };
    let candidates = shell_attrs(fragment, nix_system()?)
        .iter()
        .map(|attr| {
            let quoted = attr
                .split('.')
                .map(|name| format!("\"{}\"", name.replace('\\', "\\\\").replace('$', "\\$")))
                .collect::<Vec<_>>();
            format!("outputs.{}", quoted.join("."))
        })
        .collect::<Vec<_>>();

    let mut proc = Command::new("nix");
    proc.arg("eval");
    if read_only {
        proc.arg("--read-only");
    }
    proc.arg("--raw")
        .arg(format!("{flake_ref}#."))
        .arg("--apply")
        .arg(format!("outputs: ({}).drvPath", candidates.join(" or ")))
        .current_dir(cwd);
    capture::remove_cade_managed_env(&mut vars().collect(), &mut proc);
    Some(ShellDrv {
        installable: installable.to_owned(),
        path:        nix_stdout(&mut proc)?,
    })
}

// https://github.com/NixOS/nix/blob/16fa212a96192479593620e9b928733aaf7a2a6d/src/nix/develop.cc#L461-L477
// https://github.com/NixOS/nix/blob/16fa212a96192479593620e9b928733aaf7a2a6d/src/libcmd/installable-flake.cc#L17-L33
fn shell_attrs(fragment: &str, system: &str) -> Vec<String> {
    if fragment.is_empty() {
        return vec![
            format!("devShells.{system}.default"),
            format!("devShell.{system}"),
            format!("packages.{system}.default"),
            format!("defaultPackage.{system}"),
        ];
    }
    if let Some(attr) = fragment.strip_prefix('.') {
        return vec![attr.to_owned()];
    }
    vec![
        format!("devShells.{system}.{fragment}"),
        format!("packages.{system}.{fragment}"),
        format!("legacyPackages.{system}.{fragment}"),
        fragment.to_owned(),
    ]
}

fn nix_system() -> Option<&'static str> {
    static SYSTEM: OnceLock<Option<String>> = OnceLock::new();
    SYSTEM
        .get_or_init(|| nix_stdout(Command::new("nix").args(["config", "show", "system"])))
        .as_deref()
}

fn nix_stdout(proc: &mut Command) -> Option<String> {
    let output = proc
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
        .map(|text| text.trim_end().to_owned())
        .filter(|text| !text.is_empty())
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
            shell_drv: None,
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
    pub fn current_shell_drv(&self) -> Option<ShellDrv> {
        resolve_shell_drv(&self.cwd, &self.shell_drv.as_ref()?.installable, true)
    }

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
