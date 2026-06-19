use crate::{command::run_checked, env::EnvSet};
use anyhow::{Context, Result};
use std::{path::Path, process::Command};

pub fn call(path: &Path, argv: Vec<String>) -> Result<EnvSet> {
    let mut it = argv.iter();
    let program = it.next().context("call has no command")?;
    let mut process = Command::new(program);
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
