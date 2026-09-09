use crate::env::EnvSet;
use crate::envrc::plan::{PlannedDirective, plan_directives};
use crate::loaders::load_env;
use crate::nix::develop::{load_flake, load_shell};
use crate::types::EnvrcAction;
use crate::verbosity::{self, Verbosity};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub fn load_envrc(path: &Path, profile_dir: Option<PathBuf>) -> Result<(Vec<EnvrcAction>, EnvSet)> {
    let dir = path.parent().unwrap_or(path);
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("reading .envrc at {}", path.display()))?;
    let mut actions = Vec::new();
    let mut warnings = Vec::new();
    let mut out = EnvSet::new();

    for directive in plan_directives(dir, &contents) {
        let action = match directive.action {
            PlannedDirective::UseFlake {
                target,
                profile_name,
            } => {
                let profile = profile_dir
                    .as_ref()
                    .context("creating nix profile")?
                    .join(profile_name);
                let (dev_env, env) = load_flake(&target, &profile).context("use flake")?;
                out.merge_loaded(env);
                actions.push(EnvrcAction::NixDevEnv(dev_env));
                continue;
            }
            PlannedDirective::UseNix {
                shell,
                profile_name,
            } => {
                let profile = profile_dir
                    .as_ref()
                    .context("creating nix profile")?
                    .join(profile_name);
                let (dev_env, env) = load_shell(&shell, &profile).context("use nix")?;
                out.merge_loaded(env);
                actions.push(EnvrcAction::NixDevEnv(dev_env));
                continue;
            }
            PlannedDirective::Dotenv { path, if_exists } => EnvrcAction::Dotenv { path, if_exists },
            PlannedDirective::Export(key, value) => {
                let mut env = EnvSet::new();
                env.add_literal_export(key, &value);
                EnvrcAction::Environ(env)
            }
            PlannedDirective::PathAdd(dirs) => {
                let prefix = dirs
                    .iter()
                    .map(|entry| dir.join(entry).to_string_lossy().into_owned())
                    .collect();
                EnvrcAction::PrependPath(prefix)
            }
            PlannedDirective::WatchOnly => continue,
            PlannedDirective::Unhandled(line) => {
                warnings.push(line);
                continue;
            }
        };
        action.apply(&mut out)?;
        actions.push(action);
    }

    warn_unsupported(path, &warnings);
    Ok((actions, out))
}

pub fn activate_envrc(actions: &[EnvrcAction]) -> Result<EnvSet> {
    let mut out = EnvSet::new();
    for action in actions {
        action.apply(&mut out)?;
    }
    Ok(out)
}

impl EnvrcAction {
    fn apply(&self, out: &mut EnvSet) -> Result<()> {
        match self {
            EnvrcAction::Environ(env) => out.merge_loaded(env.clone()),
            EnvrcAction::Dotenv { path, if_exists } => {
                if !if_exists || path.exists() {
                    out.merge_loaded(load_env(path).context("dotenv")?);
                }
            }
            EnvrcAction::NixDevEnv(dev_env) => out.merge_loaded(dev_env.activate()?),
            EnvrcAction::PrependPath(prefix) => out.prepend_path_entries(prefix.clone()),
        }
        Ok(())
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

        let (_, env) = load_envrc(&path, None).unwrap();

        assert_eq!(env.derived_store_paths(), [STORE_PATH]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
