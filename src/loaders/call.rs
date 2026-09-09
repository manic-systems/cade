use std::{
    path::Path,
    process::Command,
};

use anyhow::{
    Context as _,
    Result,
};

use crate::{
    command::run_checked,
    env::set::EnvSet,
};

pub fn call(path: &Path, argv: Vec<String>) -> Result<EnvSet> {
    let cmdline = argv.join(" ");
    let mut parts = argv.into_iter();
    let program = parts.next().context("call has no command")?;
    let mut process = Command::new(program);
    process.current_dir(path);
    process.args(parts);
    let stdout = run_checked(process, &format!("call `{cmdline}`"))?;

    let text = String::from_utf8(stdout)
        .with_context(|| format!("call `{cmdline}` output must be valid UTF-8"))?;
    EnvSet::from_envs(&text)
}

#[cfg(test)]
mod tests {
    use std::env::temp_dir;

    use super::*;

    #[test]
    fn call_output_must_be_utf8() {
        let workdir = temp_dir();
        let error = call(&workdir, vec![
            "sh".into(),
            "-c".into(),
            "printf 'BAD=\\377\\n'".into(),
        ])
        .expect_err("invalid UTF-8 call output must fail");
        assert!(
            format!("{error:#}").contains("must be valid UTF-8"),
            "{error:#}"
        );
    }
}
