use crate::{
    config,
    types::EnvSet,
    verbosity::{self, Verbosity},
};
use anyhow::{Context, Result, bail};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::RecvTimeoutError,
    time::Duration,
};

const DEFAULT_LONG_RUNNING_WARNING_AFTER: Duration = Duration::from_secs(5);

fn long_running_warning_after() -> Duration {
    config::long_running_warning_ms()
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_LONG_RUNNING_WARNING_AFTER)
}

/// Run a command, returning stdout on success or an error carrying its stderr
fn run_checked(mut cmd: Command, what: &str) -> Result<Vec<u8>> {
    verbosity::log(Verbosity::Trace, format_args!("cade: running {what}."));

    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _ = tx.send(cmd.output());
    });

    let out = match rx.recv_timeout(long_running_warning_after()) {
        Ok(out) => out,
        Err(RecvTimeoutError::Timeout) => {
            verbosity::log(
                Verbosity::Normal,
                format_args!(
                    "cade: {what} is taking a long time; press Ctrl-C to stop and inspect the command."
                ),
            );
            rx.recv().context("waiting for command output")?
        }
        Err(RecvTimeoutError::Disconnected) => {
            let _ = worker.join();
            bail!("{what} failed before producing output")
        }
    };
    let _ = worker.join();
    let out = out.with_context(|| format!("running {what}"))?;
    verbosity::log(Verbosity::Trace, format_args!("cade: finished {what}."));

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr = stderr.trim();
        bail!(
            "{what} failed ({}){}",
            out.status,
            if stderr.is_empty() {
                String::new()
            } else {
                format!(":\n{stderr}")
            }
        );
    }
    Ok(out.stdout)
}

fn wipe_profile_history(profile: &Path) {
    let status = Command::new("nix")
        .args(["profile", "wipe-history", "--profile"])
        .arg(profile)
        .status();
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => verbosity::log(
            Verbosity::Trace,
            format_args!(
                "cade: failed to wipe nix profile history for {} ({status}).",
                profile.display()
            ),
        ),
        Err(e) => verbosity::log(
            Verbosity::Trace,
            format_args!(
                "cade: failed to run nix profile wipe-history for {}: {e}.",
                profile.display()
            ),
        ),
    }
}

fn nix_print_dev_env(
    path: &Path,
    args: &[String],
    what: &str,
    profile: Option<&Path>,
) -> Result<EnvSet> {
    if let Some(profile) = profile {
        match profile.parent() {
            Some(parent) => {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    verbosity::log(
                        Verbosity::Normal,
                        format_args!(
                            "cade: cannot create nix profile root at {}; activation will continue with env-scanned store roots: {e}.",
                            profile.display()
                        ),
                    );
                } else {
                    let mut proc = Command::new("nix");
                    if let Some((subcommand, rest)) = args.split_first() {
                        proc.arg(subcommand)
                            .args(["--profile"])
                            .arg(profile)
                            .args(rest);
                    } else {
                        proc.args(args).args(["--profile"]).arg(profile);
                    }
                    proc.current_dir(path);
                    match run_checked(proc, what) {
                        Ok(stdout) => {
                            let mut env = EnvSet::from_json(&stdout)?;
                            env.nix_store_paths.clear();
                            wipe_profile_history(profile);
                            return Ok(env);
                        }
                        Err(e) => verbosity::log(
                            Verbosity::Normal,
                            format_args!(
                                "cade: nix profile root at {} failed; retrying without a closure-complete root: {e:#}.",
                                profile.display()
                            ),
                        ),
                    }
                }
            }
            None => verbosity::log(
                Verbosity::Normal,
                format_args!(
                    "cade: invalid nix profile root {}; activation will continue with env-scanned store roots.",
                    profile.display()
                ),
            ),
        }
    }

    let mut proc = Command::new("nix");
    proc.args(args);
    proc.current_dir(path);
    let stdout = run_checked(proc, what)?;
    EnvSet::from_json(&stdout)
}

pub fn load_flake(path: &Path, output: Option<String>, profile: Option<PathBuf>) -> Result<EnvSet> {
    let mut args = vec!["print-dev-env".to_string(), "--json".to_string()];
    // A named output is a flake installable
    if let Some(flake_output) = output.filter(|o| !o.is_empty()) {
        args.push(format!(".#{flake_output}"));
    }
    nix_print_dev_env(
        path,
        &args,
        &format!("nix print-dev-env at {}", path.display()),
        profile.as_deref(),
    )
}

pub fn load_shell(path: &Path, filename: String, profile: Option<PathBuf>) -> Result<EnvSet> {
    let file = if filename.is_empty() {
        "./shell.nix".to_string()
    } else {
        filename
    };
    let args = vec![
        "print-dev-env".to_string(),
        "--json".to_string(),
        "-f".to_string(),
        file.clone(),
    ];
    nix_print_dev_env(
        path,
        &args,
        &format!("nix print-dev-env -f {file} at {}", path.display()),
        profile.as_deref(),
    )
}

pub fn load_env(path: &Path, filename: String) -> Result<EnvSet> {
    let mut p = path.to_path_buf();
    if filename.is_empty() {
        p.push(".env");
    } else {
        p.push(filename);
    }
    let mut file = std::fs::File::open(p)
        .with_context(|| format!("opening env file at {}", path.display()))?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).context("reading env file")?;
    EnvSet::from_envs(&buf)
}

pub fn call(path: &Path, argv: Vec<String>) -> Result<EnvSet> {
    let mut it = argv.iter();
    // safety: parser rejects an empty argv
    let mut process = Command::new(it.next().unwrap());
    process.current_dir(path);
    process.args(it);
    let cmdline = argv.join(" ");
    let stdout = run_checked(process, &format!("call `{cmdline}`"))?;

    let text = String::from_utf8(stdout)
        .with_context(|| format!("call `{cmdline}` output must be valid UTF-8"))?;
    EnvSet::from_envs(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_output_must_be_utf8() {
        let dir = std::env::temp_dir();
        let err = call(
            &dir,
            vec!["sh".into(), "-c".into(), "printf 'BAD=\\377\\n'".into()],
        )
        .expect_err("invalid UTF-8 call output must fail");
        assert!(
            format!("{err:#}").contains("must be valid UTF-8"),
            "{err:#}"
        );
    }
}
