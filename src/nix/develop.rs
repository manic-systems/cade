//! capture the final nix dev-shell process environment

use super::{capture, profile, target::FlakeTarget};
use crate::{env::EnvSet, loaders::run_checked};
use anyhow::{Context, Result};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
};

pub fn load_flake(target: &FlakeTarget, profile: Option<PathBuf>) -> Result<EnvSet> {
    let mut proc = Command::new("nix");
    proc.arg("develop");
    if !target.installable.is_empty() {
        proc.arg(&target.installable);
    }
    add_log_format(&mut proc);
    add_profile(&mut proc, profile.as_deref());
    capture::add_env_command(&mut proc);

    load_nix_dev_env(
        proc,
        &target.cwd,
        &format!("at {}", target.cwd.display()),
        profile.as_deref(),
    )
}

pub fn load_shell(file: &Path, profile: Option<PathBuf>) -> Result<EnvSet> {
    let cwd = file.parent().unwrap_or(file);
    let file_str = file.to_string_lossy();
    let mut proc = Command::new("nix");
    proc.args(["develop", "-f"]).arg(file);
    add_log_format(&mut proc);
    add_profile(&mut proc, profile.as_deref());
    capture::add_env_command(&mut proc);
    load_nix_dev_env(
        proc,
        cwd,
        &format!("-f {file_str} at {}", cwd.display()),
        profile.as_deref(),
    )
}

fn load_nix_dev_env(
    mut proc: Command,
    path: &Path,
    what: &str,
    profile: Option<&Path>,
) -> Result<EnvSet> {
    let previous_env: HashMap<_, _> = std::env::vars().collect();
    proc.current_dir(path);
    if let Some(parent) = profile.and_then(Path::parent) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating nix profile dir at {}", parent.display()))?;
    }
    let stdout = run_checked(proc, &format!("nix develop {what}"))?;
    let stdout = capture::captured_env_stdout(&stdout, what)?;
    let mut env = capture::env_set_from_captured_env(stdout, &previous_env)?;
    if let Some(profile) = profile {
        env.discard_store_paths();
        profile::wipe_history(profile);
    }
    Ok(env)
}

fn add_profile(proc: &mut Command, profile: Option<&Path>) {
    if let Some(profile) = profile {
        proc.args(["--profile"]).arg(profile);
    }
}

fn add_log_format(proc: &mut Command) {
    proc.args(["--log-format", "internal-json"]);
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
    fn find_on_path(name: &str) -> PathBuf {
        std::env::var_os("PATH")
            .and_then(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join(name))
                    .find(|candidate| candidate.is_file())
            })
            .unwrap_or_else(|| PathBuf::from(name))
    }

    #[cfg(unix)]
    #[test]
    fn nix_dev_env_capture_keeps_shell_hook_path_changes() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "cade-loader-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&root).unwrap();
        let fake_nix = root.join("nix");
        let shell = find_on_path("sh");
        let script = format!(
            r#"#!{}
set -eu
while [ "$#" -gt 0 ] && [ "$1" != "--command" ]; do
  shift
done
if [ "$#" -eq 0 ]; then
  exit 64
fi
shift
PATH="/hook/bin:/path-not-set:${{PATH:-}}"
export PATH
FROM_HOOK=ok
export FROM_HOOK
exec "$@"
"#,
            shell.display()
        );
        std::fs::write(&fake_nix, script).unwrap();
        let mut permissions = std::fs::metadata(&fake_nix).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_nix, permissions).unwrap();

        let mut proc = Command::new(&fake_nix);
        capture::add_env_command(&mut proc);
        let env = load_nix_dev_env(proc, &root, "fake nix", None).unwrap();
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(env_values(&env, "FROM_HOOK"), vec!["ok"]);
        assert_eq!(env_values(&env, "PATH"), vec!["/hook/bin"]);
    }
}
