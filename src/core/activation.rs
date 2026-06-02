use super::{
    Cade, DISALLOWED_REMINDER, Keyword, RollupResult, compute_layer_key, direnv_session_id,
    find_cade_root, is_valid_session, load_single_layer, new_session_id, rollup_envs,
    watched_files_for_keywords,
};
use crate::{
    direnv_export,
    env_delta::{EnvDelta, EnvDeltaInput, live_ambient_env},
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
        // Shell activation and JSON export share this calculation so new cade
        // semantics cannot accidentally work in one path and diverge in the
        // other.
        EnvDelta::from_rollup(EnvDeltaInput {
            env: &self.env,
            absorb: &self.absorb,
            unset: &self.unset,
            purified: self.purified,
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
        // effective root = deepest approved participant, so messages and watches track what composed
        let root = cade_files
            .last()
            .map(|(p, _)| p.clone())
            .expect("cade_files non-empty: checked above");

        let mut cade_layers = Vec::new();
        let mut all_watch_files: Vec<PathBuf> = Vec::new();
        let mut nix_store_paths: Vec<String> = Vec::new();

        for (layer_count, (path, keywords)) in cade_files.iter().enumerate() {
            let watch_files = watched_files_for_keywords(path, keywords);
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

    /// A cached layer is reusable only when its watch token still matches
    /// (enforced by `get_cached_layer`) and every nix store path it references
    /// still exists. Nix loaders are cached like any other layer, but their
    /// outputs can be collected between sessions while the inputs (and so the
    /// token) are unchanged; a missing path forces a reload that re-realizes the
    /// closure (via the nix profile) and re-roots it.
    ///
    /// The returned store paths are derived from the layer's env values rather
    /// than its cached `nix_store_paths`: the warm path has no
    /// `nix develop --profile` to root the dev-shell closure, so cade roots every
    /// referenced path itself.
    fn reusable_cached_layer(
        &self,
        dir: &str,
        token: &str,
        path: &Path,
    ) -> Result<Option<(CadeLayer, Vec<String>)>> {
        let Some(layer) = self.get_cached_layer(dir, token)? else {
            return Ok(None);
        };
        let store_paths = crate::envs::nix_store_paths_from_env_values(&layer.envs);
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
        match std::env::var("__CADE_SESSION")
            .ok()
            .filter(|s| is_valid_session(s))
        {
            Some(session) => {
                // Existing cade shells reuse their original pre-activation
                // snapshot. Using the already-mutated live env here would make
                // concat variables such as PATH grow on reload.
                let baseline = self.read_snapshot(&session).unwrap_or_else(|| live.clone());
                Ok((ActivationEnv { live, baseline }, session, false))
            }
            None => {
                // First activation creates the baseline snapshot before any
                // cade changes are emitted to the shell.
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
        let snapshot = std::env::var("__CADE_SESSION")
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
            // The shim is off, so cade exports no active project env. Still route
            // through the unwind path: if a prior shim/full export left a live
            // DIRENV_DIFF, restore its preimage instead of stranding the old
            // project's vars. With no carried diff this returns an empty no-op.
            return Ok(direnv_export::inactive_delta(export.previous));
        }
        let Some(root) = find_cade_root(&self.cwd) else {
            // This mirrors direnv's unload behavior for direct callers: leaving
            // a project must undo the last exported diff if the caller preserved
            // DIRENV_DIFF.
            return Ok(direnv_export::inactive_delta(export.previous));
        };
        // direnv can't persist __CADE_SESSION across exports, so the session is
        // derived from the holding lease or shell process. That keeps nix gc
        // roots scoped to one stable session per client, like the shell path.
        let session = direnv_session_id(client_id, owner_pid);
        let Some(plan) = self.maybe_activation_plan_for_root(root, session.as_deref())? else {
            if export.previous.is_some() {
                // A project can become disallowed while an editor still holds
                // its previous env. Restore that env instead of leaving stale
                // project variables active.
                return Ok(direnv_export::inactive_delta(export.previous));
            }
            anyhow::bail!(
                "cade project is not allowed; run `cade allow` in {}",
                self.cwd.display()
            );
        };
        if let Some(session) = session.as_deref() {
            self.root_nix_store_paths(session, &plan.nix_store_paths);
            self.refresh_session_holders(session, client_id, owner_pid);
        }
        let activation_env = ActivationEnv {
            live: export.live,
            baseline: export.baseline,
        };
        let delta = plan.rollup.env_delta(&activation_env);
        direnv_export::active_delta(delta, activation_env.baseline, export.previous)
    }
}

/// Whether every given nix store path still exists. Vacuously true when there
/// are none, so plain env and call layers always stay cacheable.
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
