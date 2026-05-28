use crate::{
    types::EnvSet,
    verbosity::{self, Verbosity},
};
use anyhow::{Context, Result, bail};
use std::{io::Read, path::Path, process::Command, sync::mpsc::RecvTimeoutError, time::Duration};

const DEFAULT_LONG_RUNNING_WARNING_AFTER: Duration = Duration::from_secs(5);

fn long_running_warning_after() -> Duration {
    std::env::var("CADE_LONG_RUNNING_WARNING_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
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

pub fn load_flake(path: &Path, output: Option<String>) -> Result<EnvSet> {
    let mut proc = Command::new("nix");
    proc.args(["print-dev-env", "--json"]);
    // A named output is a flake installable
    if let Some(flake_output) = output.filter(|o| !o.is_empty()) {
        proc.arg(format!(".#{flake_output}"));
    }
    proc.current_dir(path);
    let stdout = run_checked(proc, &format!("nix print-dev-env at {}", path.display()))?;
    EnvSet::from_json(&stdout)
}

pub fn load_shell(path: &Path, filename: String) -> Result<EnvSet> {
    let file = if filename.is_empty() {
        "./shell.nix".to_string()
    } else {
        filename
    };
    let mut proc = Command::new("nix");
    proc.args(["print-dev-env", "--json", "-f", &file]);
    proc.current_dir(path);
    let stdout = run_checked(
        proc,
        &format!("nix print-dev-env -f {file} at {}", path.display()),
    )?;
    EnvSet::from_json(&stdout)
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
