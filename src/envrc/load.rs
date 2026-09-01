use super::plan::{PlannedDirective, plan_directives};
use crate::loaders::load_env;
use crate::nix::{prepare_flake, prepare_shell};
use crate::{
    env::EnvSet,
    types::{EnvrcAction, PreparedEnvrc},
    verbosity::{self, Verbosity},
};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub fn prepare_envrc(path: &Path, profile_dir: Option<PathBuf>) -> Result<PreparedEnvrc> {
    let dir = path.parent().unwrap_or(path);
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("reading .envrc at {}", path.display()))?;

    let mut actions = Vec::new();
    let mut warnings = Vec::new();

    for directive in plan_directives(dir, &contents) {
        match directive.action {
            PlannedDirective::UseFlake {
                target,
                profile_name,
            } => {
                let profile = profile_dir.as_ref().map(|base| base.join(profile_name));
                actions.push(EnvrcAction::NixDevEnv(
                    prepare_flake(&target, profile).context("use flake")?,
                ));
            }
            PlannedDirective::UseNix {
                shell,
                profile_name,
            } => {
                let profile = profile_dir.as_ref().map(|base| base.join(profile_name));
                actions.push(EnvrcAction::NixDevEnv(
                    prepare_shell(&shell, profile).context("use nix")?,
                ));
            }
            PlannedDirective::Dotenv { path, if_exists } => {
                if if_exists && !path.exists() {
                    continue;
                }
                actions.push(EnvrcAction::Environ(load_env(&path).context("dotenv")?));
            }
            PlannedDirective::Export(key, value) => {
                let mut env = EnvSet::new();
                env.add_literal_export(key, &value);
                actions.push(EnvrcAction::Environ(env));
            }
            PlannedDirective::PathAdd(dirs) => {
                let prefix: Vec<String> = dirs
                    .iter()
                    .map(|d| dir.join(d).to_string_lossy().into_owned())
                    .collect();
                actions.push(EnvrcAction::PrependPath(prefix));
            }
            PlannedDirective::WatchOnly => {}
            PlannedDirective::Unhandled(line) => warnings.push(line),
        }
    }

    warn_unsupported(path, &warnings);
    Ok(PreparedEnvrc { actions })
}

impl PreparedEnvrc {
    pub fn replays_on_entry(&self) -> bool {
        self.actions
            .iter()
            .any(|action| matches!(action, EnvrcAction::NixDevEnv(_)))
    }

    pub fn activate(&self) -> Result<EnvSet> {
        let mut out = EnvSet::new();
        for action in &self.actions {
            match action {
                EnvrcAction::Environ(env) => out.merge_loaded(env.clone()),
                EnvrcAction::NixDevEnv(dev_env) => out.merge_loaded(dev_env.activate()?),
                EnvrcAction::PrependPath(prefix) => out.prepend_path_entries(prefix.clone()),
            }
        }
        Ok(out)
    }
}

fn warn_unsupported(path: &Path, warnings: &[String]) {
    if warnings.is_empty() || !verbosity::enabled(Verbosity::Normal) {
        return;
    }
    verbosity::log(
        Verbosity::Normal,
        format_args!(
            "cade: ignored {} unsupported line(s) in {} (not executed):",
            warnings.len(),
            path.display()
        ),
    );
    for line in warnings {
        verbosity::log(Verbosity::Normal, format_args!("    {line}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STORE_PATH: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-envrc";

    #[test]
    fn literal_export_records_store_paths() {
        let dir =
            std::env::temp_dir().join(format!("cade-envrc-store-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".envrc");
        std::fs::write(&path, format!("export TOOL={STORE_PATH}\n")).unwrap();

        let env = prepare_envrc(&path, None).unwrap().activate().unwrap();

        assert_eq!(env.derived_store_paths(), [STORE_PATH]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
