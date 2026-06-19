use crate::env::EnvSet;
use anyhow::{Context, Result};
use std::{io::Read, path::Path};

pub fn load_env(path: &Path) -> Result<EnvSet> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("opening env file at {}", path.display()))?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).context("reading env file")?;
    EnvSet::from_envs(&buf)
}
