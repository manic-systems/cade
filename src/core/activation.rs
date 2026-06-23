use super::{
    Cade, DISALLOWED_REMINDER, Keyword, find_cade_root,
    layer::load_single_layer,
    sessions::{direnv_fallback_session_id, direnv_session_id, is_valid_session, new_session_id},
    shell_state::SESSION_VAR,
    watch::{compute_layer_key, watched_files_for_keywords},
};
use crate::{
    direnv_export,
    env::{EnvDelta, EnvDeltaInput, RollupResult, live_ambient_env, rollup_envs},
    types::CadeLayer,
    verbosity::{self, Verbosity},
};
use anyhow::{Context, Result, anyhow};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

pub(super) struct ActivationPlan {
    pub(super) root: PathBuf,
    pub(super) cade_files: Vec<(PathBuf, Vec<Keyword>)>,
    pub(super) all_watch_files: Vec<PathBuf>,
    pub(super) nix_store_paths: Vec<String>,
    pub(super) rollup: RollupResult,
}

pub(super) struct ActivationEnv {
    live: HashMap<String, String>,
    baseline: HashMap<String, String>,
}

impl RollupResult {
    pub(super) fn env_delta(&self, activation_env: &ActivationEnv) -> EnvDelta {
        EnvDelta::from_rollup(EnvDeltaInput {
            env: self.env(),
            absorb: self.absorb(),
            unset: self.unset(),
            purified: self.purified(),
            live_env: &activation_env.live,
            baseline: &activation_env.baseline,
        })
    }
}

impl Cade {
    pub(super) fn activation_plan(&mut self, session: Option<&str>) -> Result<ActivationPlan> {
        let root = find_cade_root(&self.cwd)
            .context("no .cade or .envrc found in this directory or any parent")?;
        self.activation_plan_for_root(root, session)
    }

    fn activation_plan_for_root(
        &mut self,
        root: PathBuf,
        session: Option<&str>,
    ) -> Result<ActivationPlan> {
        self.maybe_activation_plan_for_root(root, session)?
            .ok_or_else(|| anyhow!("{DISALLOWED_REMINDER}"))
    }

    fn maybe_activation_plan_for_root(
        &mut self,
        root: PathBuf,
        session: Option<&str>,
    ) -> Result<Option<ActivationPlan>> {
        let cade_files = self.approved_chain(&root)?;
        if cade_files.is_empty() {
            return Ok(None);
        }
        let root = cade_files
            .last()
            .map(|(p, _)| p.clone())
            .expect("cade_files non-empty: checked above");

        let mut cade_layers = Vec::new();
        let mut all_watch_files: Vec<PathBuf> = Vec::new();
        let mut nix_store_paths: Vec<String> = Vec::new();

        for (layer_count, (path, keywords)) in cade_files.iter().enumerate() {
            let watch_files = watched_files_for_keywords(path, keywords)?;
            all_watch_files.extend(watch_files.clone());

            let token = compute_layer_key(&watch_files);
            let dir = path.to_string_lossy();

            let (layer, store_paths) = match self.reusable_cached_layer(&dir, &token, path)? {
                Some(reused) => reused,
                None => {
                    verbosity::log(
                        Verbosity::Trace,
                        format_args!("cade: loading layer {}.", path.display()),
                    );
                    let layer = load_single_layer(layer_count, path, keywords, self, session)?;
                    self.store_cached_layer(&dir, &token, &layer)?;
                    let store_paths = layer.nix_store_paths.clone();
                    (layer, store_paths)
                }
            };
            nix_store_paths.extend(store_paths);
            cade_layers.push(layer);
        }

        let rollup = rollup_envs(cade_layers);

        Ok(Some(ActivationPlan {
            root,
            cade_files,
            all_watch_files,
            nix_store_paths,
            rollup,
        }))
    }

    // nix cache hits must still point at live store paths
    fn reusable_cached_layer(
        &self,
        dir: &str,
        token: &str,
        path: &Path,
    ) -> Result<Option<(CadeLayer, Vec<String>)>> {
        let Some(layer) = self.get_cached_layer(dir, token)? else {
            return Ok(None);
        };
        let store_paths = layer.envs.derived_store_paths();
        if store_paths_all_present(&store_paths) {
            verbosity::log(
                Verbosity::Trace,
                format_args!("cade: using cached layer {}.", path.display()),
            );
            Ok(Some((layer, store_paths)))
        } else {
            verbosity::log(
                Verbosity::Trace,
                format_args!(
                    "cade: cached layer {} references missing nix store paths; reloading.",
                    path.display()
                ),
            );
            Ok(None)
        }
    }

    pub(super) fn activation_env_with_snapshot(&self) -> Result<(ActivationEnv, String, bool)> {
        let live = live_ambient_env();
        match std::env::var(SESSION_VAR)
            .ok()
            .filter(|s| is_valid_session(s))
        {
            Some(session) => {
                // never baseline from a mutated cade env
                let baseline = self.read_snapshot(&session).unwrap_or_else(|| live.clone());
                Ok((ActivationEnv { live, baseline }, session, false))
            }
            None => {
                let session = new_session_id();
                self.gc_state(None);
                self.write_snapshot(&session, &live)?;
                Ok((
                    ActivationEnv {
                        baseline: live.clone(),
                        live,
                    },
                    session,
                    true,
                ))
            }
        }
    }

    fn export_session(&self) -> direnv_export::ExportSession {
        let snapshot = std::env::var(SESSION_VAR)
            .ok()
            .and_then(|session| self.read_snapshot(&session));
        direnv_export::capture_session(snapshot)
    }

    pub fn export_env_delta(
        &mut self,
        client_id: Option<&str>,
        owner_pid: Option<u32>,
    ) -> Result<EnvDelta> {
        let export = self.export_session();
        if !crate::config::direnv_mode().runs_shim() {
            return Ok(direnv_export::inactive_delta(export.previous));
        }
        let Some(root) = find_cade_root(&self.cwd) else {
            return Ok(direnv_export::inactive_delta(export.previous));
        };
        // direnv cannot persist cade session env
        let session = direnv_session_id(client_id, owner_pid)
            .unwrap_or_else(|| direnv_fallback_session_id(&root));
        let Some(plan) = self.maybe_activation_plan_for_root(root, Some(&session))? else {
            if export.previous.is_some() {
                return Ok(direnv_export::inactive_delta(export.previous));
            }
            anyhow::bail!(
                "cade project is not allowed; run `cade allow` in {}",
                self.cwd.display()
            );
        };
        self.root_nix_store_paths(&session, &plan.nix_store_paths);
        self.refresh_session_holders(&session, client_id, owner_pid);
        let activation_env = ActivationEnv {
            live: export.live,
            baseline: export.baseline,
        };
        let metadata = direnv_export::ExportMetadata {
            root: plan.root.to_string_lossy().to_string(),
            file: direnv_export_file(&plan.root).to_string_lossy().to_string(),
            watches: plan
                .all_watch_files
                .iter()
                .map(|path| path.to_string_lossy().to_string())
                .collect(),
        };
        let delta = plan.rollup.env_delta(&activation_env);
        direnv_export::active_delta(delta, activation_env.baseline, export.previous, metadata)
    }
}

fn direnv_export_file(root: &Path) -> PathBuf {
    let cade = root.join(".cade");
    if cade.exists() {
        cade
    } else {
        root.join(".envrc")
    }
}

fn store_paths_all_present(paths: &[String]) -> bool {
    paths.iter().all(|p| Path::new(p).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_paths_all_present_is_vacuously_true_when_empty() {
        assert!(store_paths_all_present(&[]));
    }

    #[test]
    fn store_paths_all_present_detects_a_missing_path() {
        let dir = std::env::temp_dir().join(format!(
            "cade-storepaths-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let present = dir.join("present");
        std::fs::write(&present, b"").unwrap();
        let present = present.to_string_lossy().to_string();
        let missing = dir.join("missing").to_string_lossy().to_string();

        assert!(store_paths_all_present(std::slice::from_ref(&present)));
        assert!(!store_paths_all_present(&[present, missing]));

        std::fs::remove_dir_all(&dir).ok();
    }
}
