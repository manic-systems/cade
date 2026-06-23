use super::filter::{is_kept_nix_env_var, keep_loaded_env_var};
use crate::{
    core::shell_state::{SET_VAR, decode_key_list},
    env::EnvSet,
};
use anyhow::{Context, Result, bail};
use std::{collections::HashMap, path::PathBuf, process::Command};

const ENV_MARKER: &[u8] = b"\0__CADE_ENV_BEGIN__\0";
const ENV_CAPTURE_SCRIPT: &str = "printf '\\0__CADE_ENV_BEGIN__\\0'\nexec \"$1\" -0";

pub(super) fn add_env_command(proc: &mut Command) {
    proc.args(["--command"])
        .arg(find_on_path("sh"))
        .args(["-c", ENV_CAPTURE_SCRIPT, "cade-env"])
        .arg(find_on_path("env"));
}

pub(super) fn remove_cade_managed_env(previous: &mut HashMap<String, String>, proc: &mut Command) {
    for key in cade_managed_env_keys(previous) {
        previous.remove(&key);
        proc.env_remove(&key);
    }
}

fn cade_managed_env_keys(env: &HashMap<String, String>) -> Vec<String> {
    let mut keys: Vec<String> = env
        .keys()
        .filter(|key| key.starts_with("__CADE_"))
        .cloned()
        .collect();

    if let Some(set) = env.get(SET_VAR) {
        keys.extend(
            decode_key_list(set)
                .into_iter()
                .filter(|key| !key.is_empty() && key != "PATH"),
        );
    }

    keys.sort_unstable();
    keys.dedup();
    keys
}

fn find_on_path(name: &str) -> PathBuf {
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(name))
                .find(|candidate| candidate.is_file())
        })
        .unwrap_or_else(|| PathBuf::from(name))
}

pub(super) fn captured_env_stdout<'a>(stdout: &'a [u8], what: &str) -> Result<&'a [u8]> {
    let Some(start) = stdout
        .windows(ENV_MARKER.len())
        .position(|window| window == ENV_MARKER)
    else {
        bail!("nix develop {what} did not emit a captured environment marker");
    };

    Ok(&stdout[start + ENV_MARKER.len()..])
}

pub(super) fn env_set_from_captured_env(
    raw: &[u8],
    previous: &HashMap<String, String>,
) -> Result<EnvSet> {
    let path_suffix = previous.get("PATH").map(String::as_str);
    let mut vars: HashMap<String, Vec<String>> = HashMap::new();
    let mut seen = std::collections::HashSet::new();

    for entry in raw.split(|&b| b == b'\0').filter(|entry| !entry.is_empty()) {
        let text = std::str::from_utf8(entry).context("parsing exported environment")?;
        let Some((key, raw_value)) = text.split_once('=') else {
            continue;
        };
        seen.insert(key.to_string());
        if !keep_loaded_env_var(key) {
            continue;
        }
        if previous.get(key).is_some_and(|value| value == raw_value) && !is_kept_nix_env_var(key) {
            continue;
        }

        let value = if key == "PATH" {
            clean_captured_path(raw_value, path_suffix)
        } else {
            raw_value.to_string()
        };
        if key == "PATH" && value.is_empty() {
            continue;
        }

        vars.insert(
            key.to_string(),
            value.split(':').map(|s| s.to_string()).collect(),
        );
    }

    let clears = previous
        .keys()
        .filter(|key| !seen.contains(*key) && keep_loaded_env_var(key))
        .cloned()
        .collect();

    Ok(EnvSet::from_captured_parts(vars, clears))
}

fn clean_captured_path(value: &str, path_suffix: Option<&str>) -> String {
    let mut parts: Vec<&str> = value
        .split(':')
        .filter(|part| !part.is_empty() && *part != "/path-not-set")
        .collect();

    if let Some(suffix) = path_suffix {
        let suffix_parts: Vec<&str> = suffix
            .split(':')
            .filter(|part| !part.is_empty() && *part != "/path-not-set")
            .collect();
        if !suffix_parts.is_empty() && parts.ends_with(&suffix_parts) {
            parts.truncate(parts.len() - suffix_parts.len());
        }
    }

    parts.join(":")
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

    fn env_contains(env: &EnvSet, key: &str) -> bool {
        serde_json::to_value(env).unwrap()["vars"]
            .as_object()
            .unwrap()
            .contains_key(key)
    }

    #[test]
    fn cade_managed_env_keys_drop_active_shell_vars_but_keep_path_suffix() {
        let env = HashMap::from([
            ("__CADE_SESSION".to_string(), "s1".to_string()),
            ("__CADE_SET".to_string(), "FOO\x1FPATH\x1FBAR".to_string()),
            ("FOO".to_string(), "old".to_string()),
            ("PATH".to_string(), "/old/bin".to_string()),
            ("BAR".to_string(), "old".to_string()),
        ]);

        assert_eq!(
            cade_managed_env_keys(&env),
            ["BAR", "FOO", "__CADE_SESSION", "__CADE_SET"]
        );
    }

    #[test]
    fn add_env_command_uses_resolved_env_binary() {
        let mut proc = Command::new("nix");
        add_env_command(&mut proc);
        let args: Vec<_> = proc.get_args().map(|arg| arg.to_owned()).collect();

        assert_eq!(args[0], "--command");
        assert!(std::path::Path::new(&args[1]).is_absolute() || args[1] == "sh");
        assert_eq!(args[2], "-c");
        assert_eq!(args[3], ENV_CAPTURE_SCRIPT);
        assert_eq!(args[4], "cade-env");
        assert!(std::path::Path::new(&args[5]).is_absolute() || args[5] == "env");
    }

    #[test]
    fn captured_env_stdout_skips_hook_output() {
        let stdout = b"hello from hook\n\0__CADE_ENV_BEGIN__\0PATH=/dev/bin\0";
        assert_eq!(
            captured_env_stdout(stdout, "test").unwrap(),
            b"PATH=/dev/bin\0"
        );
    }

    #[test]
    fn captured_env_strips_runner_path_suffix_and_nix_sentinel() {
        let previous = HashMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]);
        let env = env_set_from_captured_env(
            b"PATH=/dev/bin:/path-not-set:/usr/bin:/bin\0FOO=bar\0",
            &previous,
        )
        .unwrap();

        assert_eq!(env_values(&env, "PATH"), vec!["/dev/bin"]);
        assert_eq!(env_values(&env, "FOO"), vec!["bar"]);
    }

    #[test]
    fn captured_env_keeps_unchanged_nix_wrapper_vars() {
        let previous = HashMap::from([
            ("NIX_CC".to_string(), "/nix/store/gcc-wrapper".to_string()),
            ("PKG_CONFIG_PATH".to_string(), "/old/pkgconfig".to_string()),
            ("AMBIENT".to_string(), "same".to_string()),
        ]);
        let env = env_set_from_captured_env(
            b"NIX_CC=/nix/store/gcc-wrapper\0PKG_CONFIG_PATH=/old/pkgconfig\0AMBIENT=same\0",
            &previous,
        )
        .unwrap();

        assert_eq!(env_values(&env, "NIX_CC"), vec!["/nix/store/gcc-wrapper"]);
        assert!(!env_contains(&env, "PKG_CONFIG_PATH"));
        assert!(!env_contains(&env, "AMBIENT"));
    }
}
