use crate::env::set::EnvSet;
use anyhow::{Context as _, Result};
use std::{fs::read_to_string, path::Path};

pub fn load_env(path: &Path) -> Result<EnvSet> {
    let buf =
        read_to_string(path).with_context(|| format!("opening env file at {}", path.display()))?;
    EnvSet::from_envs(&buf)
}
