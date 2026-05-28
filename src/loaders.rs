use crate::types::EnvSet;
use anyhow::{Context, Result, bail};
use std::{io::Read, path::Path, process::Command};

/// Run a command, returning stdout on success or an error carrying its stderr
fn run_checked(mut cmd: Command, what: &str) -> Result<Vec<u8>> {
    let out = cmd.output().with_context(|| format!("running {what}"))?;
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
