use super::{
    Cade, DISALLOWED_REMINDER, Keyword, RollupResult, compute_layer_key, find_cade_root,
    load_single_layer, new_session_id, rollup_envs, watched_files_for_keywords,
};
use crate::{
    direnv_export,
    env_delta::{EnvDelta, EnvDeltaInput, live_ambient_env},
    verbosity::{self, Verbosity},
};
use anyhow::{Context, Result, anyhow};
use std::{collections::HashMap, path::PathBuf};

pub(super) struct ActivationPlan {
    pub(super) root: PathBuf,
    pub(super) cade_files: Vec<(PathBuf, Vec<Keyword>)>,
    pub(super) all_watch_files: Vec<PathBuf>,
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
    pub(super) fn activation_plan(&mut self) -> Result<ActivationPlan> {
        let root = find_cade_root(&self.cwd)
            .context("no .cade or .envrc found in this directory or any parent")?;
        self.activation_plan_for_root(root)
    }

    fn activation_plan_for_root(&mut self, root: PathBuf) -> Result<ActivationPlan> {
        self.maybe_activation_plan_for_root(root)?
            .ok_or_else(|| anyhow!("{DISALLOWED_REMINDER}"))
    }

    fn maybe_activation_plan_for_root(&mut self, root: PathBuf) -> Result<Option<ActivationPlan>> {
        let cade_files = self.approved_chain(&root)?;
        if cade_files.is_empty() {
            return Ok(None);
        }

        let mut cade_layers = Vec::new();
        let mut all_watch_files: Vec<PathBuf> = Vec::new();

        for (layer_count, (path, keywords)) in cade_files.iter().enumerate() {
            let watch_files = watched_files_for_keywords(path, keywords);
            all_watch_files.extend(watch_files.clone());

            let token = compute_layer_key(&watch_files);
            let dir = path.to_string_lossy();
            if let Some(cached) = self.get_cached_layer(&dir, &token)? {
                verbosity::log(
                    Verbosity::Trace,
                    format_args!("cade: using cached layer {}.", path.display()),
                );
                cade_layers.push(cached);
            } else {
                verbosity::log(
                    Verbosity::Trace,
                    format_args!("cade: loading layer {}.", path.display()),
                );
                let layer = load_single_layer(layer_count, path, keywords)?;
                self.store_cached_layer(&dir, &token, &layer)?;
                cade_layers.push(layer);
            }
        }

        let rollup = rollup_envs(cade_layers);

        Ok(Some(ActivationPlan {
            root,
            cade_files,
            all_watch_files,
            rollup,
        }))
    }

    pub(super) fn activation_env_with_snapshot(&self) -> Result<(ActivationEnv, Option<String>)> {
        let live = live_ambient_env();
        match std::env::var("__CADE_SESSION") {
            Ok(session) => {
                // Existing cade shells reuse their original pre-activation
                // snapshot. Using the already-mutated live env here would make
                // concat variables such as PATH grow on reload.
                let baseline = self.read_snapshot(&session).unwrap_or_else(|| live.clone());
                Ok((ActivationEnv { live, baseline }, None))
            }
            Err(_) => {
                // First activation creates the baseline snapshot before any
                // cade changes are emitted to the shell.
                let session = new_session_id();
                self.gc_snapshots();
                self.write_snapshot(&session, &live)?;
                Ok((
                    ActivationEnv {
                        baseline: live.clone(),
                        live,
                    },
                    Some(session),
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

    pub fn export_env_delta(&mut self) -> Result<EnvDelta> {
        let export = self.export_session();
        let Some(root) = find_cade_root(&self.cwd) else {
            // This mirrors direnv's unload behavior for direct callers: leaving
            // a project must undo the last exported diff if the caller preserved
            // DIRENV_DIFF.
            return Ok(direnv_export::inactive_delta(export.previous));
        };
        let Some(plan) = self.maybe_activation_plan_for_root(root)? else {
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
        let activation_env = ActivationEnv {
            live: export.live,
            baseline: export.baseline,
        };
        let delta = plan.rollup.env_delta(&activation_env);
        direnv_export::active_delta(delta, activation_env.baseline, export.previous)
    }
}
