use super::plan::{PlannedDirective, plan_directives};
use crate::loaders::load_env;
use crate::nix::{load_flake, load_shell};
use crate::{
    env::EnvSet,
    verbosity::{self, Verbosity},
};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub fn load_envrc(path: &Path, profile_dir: Option<PathBuf>) -> Result<EnvSet> {
    let dir = path.parent().unwrap_or(path);
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("reading .envrc at {}", path.display()))?;

    let mut out = EnvSet::new();
    let mut warnings = Vec::new();

    for directive in plan_directives(dir, &contents) {
        match directive.action {
            PlannedDirective::UseFlake {
                target,
                profile_name,
            } => {
                let profile = profile_dir.as_ref().map(|base| base.join(profile_name));
                out.merge_loaded(load_flake(&target, profile).context("use flake")?);
            }
            PlannedDirective::UseNix {
                shell,
                profile_name,
            } => {
                let profile = profile_dir.as_ref().map(|base| base.join(profile_name));
                out.merge_loaded(load_shell(&shell, profile).context("use nix")?);
            }
            PlannedDirective::Dotenv { path, if_exists } => {
                if if_exists && !path.exists() {
                    continue;
                }
                out.merge_loaded(load_env(&path).context("dotenv")?);
            }
            PlannedDirective::Export(key, value) => {
                out.add_literal_export(key, &value);
            }
            PlannedDirective::PathAdd(dirs) => {
                let prefix: Vec<String> = dirs
                    .iter()
                    .map(|d| dir.join(d).to_string_lossy().into_owned())
                    .collect();
                out.prepend_path_entries(prefix);
            }
            PlannedDirective::WatchOnly => {}
            PlannedDirective::Unhandled(line) => warnings.push(line),
        }
    }

    warn_unsupported(path, &warnings);
    Ok(out)
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

        let env = load_envrc(&path, None).unwrap();

        assert_eq!(env.derived_store_paths(), [STORE_PATH]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
