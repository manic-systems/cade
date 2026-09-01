use super::{capture, profile, target::FlakeTarget};
use crate::{
    command::{run_checked, run_checked_output},
    env::EnvSet,
    types::NixDevEnv,
};
use anyhow::{Context, Result};
use std::{
    collections::HashMap,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

pub fn prepare_flake(target: &FlakeTarget, profile: Option<PathBuf>) -> Result<NixDevEnv> {
    let mut proc = Command::new("nix");
    proc.arg("print-dev-env");
    if !target.installable.is_empty() {
        proc.arg(&target.installable);
    }
    add_log_format(&mut proc);
    add_profile(&mut proc, profile.as_deref());

    prepare_nix_dev_env(
        proc,
        &target.cwd,
        &format!("at {}", target.cwd.display()),
        profile.as_deref(),
    )
}

pub fn prepare_shell(file: &Path, profile: Option<PathBuf>) -> Result<NixDevEnv> {
    let cwd = file.parent().unwrap_or(file);
    let file_str = file.to_string_lossy();
    let mut proc = Command::new("nix");
    proc.args(["print-dev-env", "-f"]).arg(file);
    add_log_format(&mut proc);
    add_profile(&mut proc, profile.as_deref());
    prepare_nix_dev_env(
        proc,
        cwd,
        &format!("-f {file_str} at {}", cwd.display()),
        profile.as_deref(),
    )
}

fn prepare_nix_dev_env(
    mut proc: Command,
    path: &Path,
    what: &str,
    profile: Option<&Path>,
) -> Result<NixDevEnv> {
    proc.current_dir(path);
    if let Some(parent) = profile.and_then(Path::parent) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating nix profile dir at {}", parent.display()))?;
    }
    let stdout = run_checked(proc, &format!("nix print-dev-env {what}"))?;
    let script = String::from_utf8(stdout).context("reading nix dev environment script")?;
    if let Some(profile) = profile {
        profile::wipe_history(profile);
    }
    Ok(NixDevEnv {
        script,
        cwd: path.to_path_buf(),
    })
}

impl NixDevEnv {
    pub fn activate(&self) -> Result<EnvSet> {
        let mut previous_env: HashMap<_, _> = std::env::vars().collect();
        let mut proc = Command::new(find_on_path("bash"));
        let script = format!("{}\n{}", self.script, capture::env_capture_script());
        proc.args(["-c", &script, "cade-dev-env"])
            .arg(find_on_path("env"))
            .current_dir(&self.cwd);
        capture::remove_cade_managed_env(&mut previous_env, &mut proc);

        let output = run_checked_output(
            proc,
            &format!("entering nix dev shell at {}", self.cwd.display()),
        )?;
        let (hook_stdout, raw_env) =
            capture::captured_env_output(&output.stdout, &format!("at {}", self.cwd.display()))?;
        let mut stderr = std::io::stderr().lock();
        stderr
            .write_all(hook_stdout)
            .context("write nix shellHook output")?;
        stderr
            .write_all(&output.stderr)
            .context("write nix shellHook error output")?;
        capture::env_set_from_captured_env(raw_env, &previous_env)
    }
}

fn add_profile(proc: &mut Command, profile: Option<&Path>) {
    if let Some(profile) = profile {
        proc.args(["--profile"]).arg(profile);
    }
}

fn add_log_format(proc: &mut Command) {
    proc.args(["--log-format", "internal-json"]);
}

fn find_on_path(name: &str) -> PathBuf {
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(name))
                .find(|candidate| {
                    candidate.metadata().is_ok_and(|metadata| {
                        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                    })
                })
        })
        .unwrap_or_else(|| PathBuf::from(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_values(env: &EnvSet, key: &str) -> Vec<String> {
        serde_json::to_value(env).unwrap()["vars"][key]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_string())
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn cached_nix_dev_env_replays_shell_hook_changes() {
        let root = std::env::temp_dir().join(format!(
            "cade-loader-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&root).unwrap();
        let hook_log = root.join("hook.log");
        let script = format!(
            r#"shellHook='printf ran\\n >> "{}"
PATH="/hook/bin:/path-not-set:${{PATH:-}}"
export PATH
FROM_HOOK=ok
export FROM_HOOK'
export shellHook
eval "${{shellHook:-}}"
"#,
            hook_log.display()
        );
        let dev_env = NixDevEnv {
            script,
            cwd: root.clone(),
        };
        let env = dev_env.activate().unwrap();
        let second = dev_env.activate().unwrap();

        assert_eq!(std::fs::read_to_string(&hook_log).unwrap(), "ran\nran\n");
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(env_values(&env, "FROM_HOOK"), vec!["ok"]);
        assert_eq!(env_values(&env, "PATH"), vec!["/hook/bin"]);
        assert_eq!(env_values(&second, "FROM_HOOK"), vec!["ok"]);
    }
}
