//! Capture the final Nix dev-shell process environment.
//!
//! `nix print-dev-env --json` is convenient, but it describes the build
//! environment before bash evaluates shell setup such as devshell/shellHook PATH
//! changes. The non-JSON `print-dev-env` shell script can produce the right
//! result, but evaluating it correctly would make cade run bash itself. Instead,
//! `nix develop --command` lets Nix own that setup, and cade only dumps the
//! resulting process environment.

use crate::{loaders::run_checked, types::EnvSet};
use anyhow::{Context, Result, bail};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
};

const ENV_MARKER: &[u8] = b"\0__CADE_ENV_BEGIN__\0";
const ENV_CAPTURE_SCRIPT: &str = "printf '\\0__CADE_ENV_BEGIN__\\0'\nexec \"$1\" -0";

// Nix exposes many derivation/build variables in a dev shell. Cade only wants
// user-facing environment changes, so keep the historical filter from the old
// `print-dev-env --json` path and apply it to the captured env dump.
const IGNORED_ENV_PREFIXES: &[&str] = &["NIX_", "output", "deps", "enable"];
const IGNORED_ENV_SUFFIXES: &[&str] = &["Inputs", "Flags", "TYPE"];
const IGNORED_ENV_KEYS: &[&str] = &[
    "SHELL",
    "pkg",
    "prefix",
    "guess",
    "_substituteStream_has_warned_replace_deprecation",
    "LINENO",
    "OPTERROR",
    "OLDPWD",
    "BASH",
    "IFS",
    "PS4",
    "initialPath",
    "out",
    "shell",
    "STRINGS",
    "stdenv",
    "builder",
    "PWD",
    "SOURCE_DATE_EPOCH",
    "CXX",
    "TEMPDIR",
    "system",
    "HOST_PATH",
    "doInstallCheck",
    "buildCommandPath",
    "LS_COLORS",
    "cmakeFlakes",
    "TMPDIR",
    "LD",
    "READELF",
    "doCheck",
    "SIZE",
    "propagatedNativeBuildInputs",
    "strictDeps",
    "AR",
    "AS",
    "TEMP",
    "SHLVL",
    "NM",
    "patches",
    "passAsFile",
    "buildInputs",
    "SSL_CERT_FILE",
    "OBJCOPY",
    "STRIP",
    "TMP",
    "OBJDUMP",
    "propagatedBuildInputs",
    "CC",
    "__ETC_PROFILE_SOURCED",
    "CONFIG_SHELL",
    "__structuredAttrs",
    "RANLIB",
    "nativeBuildInputs",
    "name",
    "TEST",
    "TZ",
    "HOME",
    "GZIP_NO_TIMESTAMPS",
    "cmakeFlags",
    "TERM",
    "buildCommand",
    "preferLocalBuild",
    "dontAddDisableDepTrack",
];

pub(crate) fn load_flake(
    path: &Path,
    output: Option<String>,
    profile: Option<PathBuf>,
) -> Result<EnvSet> {
    let mut proc = Command::new("nix");
    proc.arg("develop");
    // A named output is a flake installable.
    if let Some(flake_output) = output.filter(|o| !o.is_empty()) {
        proc.arg(format!(".#{flake_output}"));
    }
    add_profile(&mut proc, profile.as_deref());
    add_env_command(&mut proc);

    load_nix_dev_env(
        proc,
        path,
        &format!("at {}", path.display()),
        profile.as_deref(),
    )
}

pub(crate) fn load_shell(
    path: &Path,
    filename: String,
    profile: Option<PathBuf>,
) -> Result<EnvSet> {
    let file = if filename.is_empty() {
        "./shell.nix".to_string()
    } else {
        filename
    };
    let mut proc = Command::new("nix");
    proc.args(["develop", "-f", &file]);
    add_profile(&mut proc, profile.as_deref());
    add_env_command(&mut proc);
    load_nix_dev_env(
        proc,
        path,
        &format!("-f {file} at {}", path.display()),
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
    let stdout = run_checked(proc, &format!("nix develop {what}"))?;
    let stdout = captured_env_stdout(&stdout, what)?;
    let mut env = env_set_from_captured_env(stdout, &previous_env)?;
    if let Some(profile) = profile {
        env.nix_store_paths.clear();
        wipe_profile_history(profile);
    }
    Ok(env)
}

fn add_profile(proc: &mut Command, profile: Option<&Path>) {
    if let Some(profile) = profile {
        proc.args(["--profile"]).arg(profile);
    }
}

fn add_env_command(proc: &mut Command) {
    // Use absolute sh/env paths when possible. Once inside `nix develop`,
    // PATH may intentionally be incomplete or rewritten by the shell setup.
    proc.args(["--command"])
        .arg(find_on_path("sh"))
        .args(["-c", ENV_CAPTURE_SCRIPT, "cade-env"])
        .arg(find_on_path("env"));
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

fn captured_env_stdout<'a>(stdout: &'a [u8], what: &str) -> Result<&'a [u8]> {
    // Hooks may print banners or warnings before our command runs. The NUL
    // marker makes the env dump unambiguous without imposing silence on hooks.
    let Some(start) = stdout
        .windows(ENV_MARKER.len())
        .position(|window| window == ENV_MARKER)
    else {
        bail!("nix develop {what} did not emit a captured environment marker");
    };

    Ok(&stdout[start + ENV_MARKER.len()..])
}

fn env_set_from_captured_env(raw: &[u8], previous: &HashMap<String, String>) -> Result<EnvSet> {
    let path_suffix = previous.get("PATH").map(String::as_str);
    let mut vars: HashMap<String, Vec<String>> = HashMap::new();

    // env -0 avoids newline/shell quoting ambiguities. Environment variables
    // cannot contain NUL bytes, so this is the lossless delimiter available to
    // ordinary Unix process environments.
    for entry in raw.split(|&b| b == b'\0').filter(|entry| !entry.is_empty()) {
        let text = std::str::from_utf8(entry).context("parsing exported environment")?;
        let Some((key, raw_value)) = text.split_once('=') else {
            continue;
        };
        if !keep_loaded_env_var(key) {
            continue;
        }
        if previous.get(key).is_some_and(|value| value == raw_value) {
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

    let mut env = EnvSet::from_vars(vars);
    env.nix_store_paths = crate::envs::nix_store_paths_from_env_values(&env);
    Ok(env)
}

fn wipe_profile_history(profile: &Path) {
    let status = Command::new("nix")
        .args(["profile", "wipe-history", "--profile"])
        .arg(profile)
        .status();
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => crate::verbosity::log(
            crate::verbosity::Verbosity::Trace,
            format_args!(
                "cade: failed to wipe nix profile history for {} ({status}).",
                profile.display()
            ),
        ),
        Err(e) => crate::verbosity::log(
            crate::verbosity::Verbosity::Trace,
            format_args!(
                "cade: failed to run nix profile wipe-history for {}: {e}.",
                profile.display()
            ),
        ),
    }
}

fn keep_loaded_env_var(var: &str) -> bool {
    !(IGNORED_ENV_PREFIXES
        .iter()
        .any(|prefix| var.starts_with(prefix))
        || IGNORED_ENV_SUFFIXES
            .iter()
            .any(|suffix| var.ends_with(suffix))
        || var.to_lowercase().contains("phase")
        || IGNORED_ENV_KEYS.contains(&var))
}

fn clean_captured_path(value: &str, path_suffix: Option<&str>) -> String {
    let mut parts: Vec<&str> = value
        .split(':')
        .filter(|part| !part.is_empty() && *part != "/path-not-set")
        .collect();

    if let Some(suffix) = path_suffix {
        // `nix develop --command` appends the runner's PATH after the dev-shell
        // entries. Cade later concatenates with the ambient PATH itself, so
        // keeping this suffix would duplicate the caller's PATH on activation.
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

    #[test]
    fn add_env_command_uses_resolved_env_binary() {
        let mut proc = Command::new("nix");
        add_env_command(&mut proc);
        let args: Vec<_> = proc.get_args().map(|arg| arg.to_owned()).collect();

        assert_eq!(args[0], "--command");
        assert!(Path::new(&args[1]).is_absolute() || args[1] == "sh");
        assert_eq!(args[2], "-c");
        assert_eq!(args[3], ENV_CAPTURE_SCRIPT);
        assert_eq!(args[4], "cade-env");
        assert!(Path::new(&args[5]).is_absolute() || args[5] == "env");
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

        assert_eq!(env.vars["PATH"], vec!["/dev/bin"]);
        assert_eq!(env.vars["FOO"], vec!["bar"]);
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
        add_env_command(&mut proc);
        let env = load_nix_dev_env(proc, &root, "fake nix", None).unwrap();
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(env.vars["FROM_HOOK"], vec!["ok"]);
        assert_eq!(env.vars["PATH"], vec!["/hook/bin"]);
    }
}
