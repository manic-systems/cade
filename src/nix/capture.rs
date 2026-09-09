use super::filter::{is_kept_nix_env_var, keep_loaded_env_var};
use crate::{
    core::shell_state::{SET_VAR, decode_key_list},
    env::set::EnvSet,
};
use anyhow::{Context as _, Result, bail};
use std::{
    collections::{BTreeMap, BTreeSet},
    process::Command,
};

const ENV_MARKER: &[u8] = b"\0__CADE_ENV_BEGIN__\0";
const ENV_CAPTURE_SCRIPT: &str = "printf '\\0__CADE_ENV_BEGIN__\\0'\nexec \"$1\" -0";

pub(super) const fn env_capture_script() -> &'static str {
    ENV_CAPTURE_SCRIPT
}

pub(super) fn remove_cade_managed_env(previous: &mut BTreeMap<String, String>, proc: &mut Command) {
    for key in cade_managed_env_keys(previous) {
        previous.remove(&key);
        proc.env_remove(&key);
    }
}

fn cade_managed_env_keys(env: &BTreeMap<String, String>) -> Vec<String> {
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

pub(super) fn captured_env_output<'output>(
    stdout: &'output [u8],
    what: &str,
) -> Result<(&'output [u8], &'output [u8])> {
    let Some(start) = stdout
        .windows(ENV_MARKER.len())
        .position(|window| window == ENV_MARKER)
    else {
        bail!("nix develop {what} did not emit a captured environment marker");
    };

    Ok((&stdout[..start], &stdout[start + ENV_MARKER.len()..]))
}

pub(super) fn env_set_from_captured_env(
    raw: &[u8],
    previous: &BTreeMap<String, String>,
) -> Result<EnvSet> {
    let path_suffix = previous.get("PATH").map(String::as_str);
    let mut vars: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut seen = BTreeSet::new();

    for entry in raw
        .split(|&byte| byte == b'\0')
        .filter(|entry| !entry.is_empty())
    {
        let text = str::from_utf8(entry).context("parsing exported environment")?;
        let Some((key, raw_value)) = text.split_once('=') else {
            continue;
        };
        seen.insert(key.to_owned());
        if !keep_loaded_env_var(key) {
            continue;
        }
        if previous.get(key).is_some_and(|value| value == raw_value) && !is_kept_nix_env_var(key) {
            continue;
        }

        let value = if key == "PATH" {
            clean_captured_path(raw_value, path_suffix)
        } else {
            raw_value.to_owned()
        };
        if key == "PATH" && value.is_empty() {
            continue;
        }

        vars.insert(
            key.to_owned(),
            value.split(':').map(ToOwned::to_owned).collect(),
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
            .map(|value| value.as_str().unwrap().to_owned())
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
        let env = BTreeMap::from([
            ("__CADE_SESSION".to_owned(), "s1".to_owned()),
            ("__CADE_SET".to_owned(), "FOO\x1FPATH\x1FBAR".to_owned()),
            ("FOO".to_owned(), "old".to_owned()),
            ("PATH".to_owned(), "/old/bin".to_owned()),
            ("BAR".to_owned(), "old".to_owned()),
        ]);

        assert_eq!(
            cade_managed_env_keys(&env),
            ["BAR", "FOO", "__CADE_SESSION", "__CADE_SET"]
        );
    }

    #[test]
    fn captured_env_stdout_skips_hook_output() {
        let stdout = b"hello from hook\n\0__CADE_ENV_BEGIN__\0PATH=/dev/bin\0";
        assert_eq!(
            captured_env_output(stdout, "test").unwrap().1,
            b"PATH=/dev/bin\0"
        );
    }

    #[test]
    fn captured_env_strips_runner_path_suffix_and_nix_sentinel() {
        let previous = BTreeMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned())]);
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
        let previous = BTreeMap::from([
            ("NIX_CC".to_owned(), "/nix/store/gcc-wrapper".to_owned()),
            ("PKG_CONFIG_PATH".to_owned(), "/old/pkgconfig".to_owned()),
            ("AMBIENT".to_owned(), "same".to_owned()),
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
