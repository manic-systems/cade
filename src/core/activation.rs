use crate::{
    config::direnv_mode,
    core::{
        Cade, DISALLOWED_REMINDER,
        cache::{get_cached_layer, store_cached_layer},
        layer::load_single_layer,
        participants::find_cade_root,
        permissions::approved_chain,
        sessions::{
            direnv_fallback_session_id, direnv_session_id,
            gc_roots::{gc_state, refresh_session_holders, root_nix_store_paths},
            is_valid_session, new_session_id,
        },
        shell_state::SESSION_VAR,
        snapshot::{read_snapshot, write_snapshot},
        watch::layer_watch,
    },
    direnv_export,
    env::{
        delta::{EnvDelta, EnvDeltaInput, live_ambient_env},
        rollup::{RollupResult, rollup_envs},
    },
    types::{keyword::Keyword, layer::CadeLayer},
    verbosity::{self, Verbosity},
};
use anyhow::{Context as _, Result, anyhow};
use std::{
    collections::BTreeMap,
    env::var,
    path::{Path, PathBuf},
};

pub(super) struct ActivationPlan {
    pub root: PathBuf,
    pub cade_files: Vec<(PathBuf, Vec<Keyword>)>,
    pub all_watch_files: Vec<PathBuf>,
    pub nix_store_paths: Vec<String>,
    pub rollup: RollupResult,
}

pub(super) struct ActivationEnv {
    live: BTreeMap<String, String>,
    baseline: BTreeMap<String, String>,
}

impl ActivationEnv {
    pub(super) fn delta(&self, rollup: &RollupResult) -> EnvDelta {
        EnvDelta::from_rollup(EnvDeltaInput {
            env: rollup.env(),
            absorb: rollup.absorb(),
            unset: rollup.unset(),
            purified: rollup.purified(),
            live_env: &self.live,
            baseline: &self.baseline,
        })
    }
}

pub(super) fn activation_plan(cade: &Cade, session: &str) -> Result<ActivationPlan> {
    let root = find_cade_root(&cade.cwd)
        .context("no .cade or .envrc found in this directory or any parent")?;
    activation_plan_for_root(cade, &root, session)
}

fn activation_plan_for_root(cade: &Cade, root: &Path, session: &str) -> Result<ActivationPlan> {
    maybe_activation_plan_for_root(cade, root, session)?
        .ok_or_else(|| anyhow!("{DISALLOWED_REMINDER}"))
}

fn maybe_activation_plan_for_root(
    cade: &Cade,
    root: &Path,
    session: &str,
) -> Result<Option<ActivationPlan>> {
    let cade_files = approved_chain(cade, root)?;
    if cade_files.is_empty() {
        return Ok(None);
    }
    let effective_root = cade_files
        .last()
        .map(|entry| entry.0.clone())
        .expect("cade_files non-empty: checked above");

    let mut cade_layers = Vec::new();
    let mut all_watch_files: Vec<PathBuf> = Vec::new();
    let mut nix_store_paths: Vec<String> = Vec::new();

    for (layer_count, layer_entry) in cade_files.iter().enumerate() {
        let path = &layer_entry.0;
        let keywords = &layer_entry.1;
        let (watch_files, token) = layer_watch(cade, path, keywords)?;
        all_watch_files.extend(watch_files);

        let dir = path.to_string_lossy();

        let (layer, store_paths) = if let Some(reused) =
            reusable_cached_layer(cade, &dir, &token, path)?
        {
            reused
        } else {
            verbosity::log(
                Verbosity::Trace,
                format_args!("cade: loading layer {}.", path.display()),
            );
            let (cached, layer) = load_single_layer(layer_count, path, keywords, cade, session)?;
            store_cached_layer(cade, &dir, &token, &cached)?;
            let store_paths = layer.nix_store_paths.clone();
            (layer, store_paths)
        };
        nix_store_paths.extend(store_paths);
        cade_layers.push(layer);
    }

    let rollup = rollup_envs(cade_layers);

    Ok(Some(ActivationPlan {
        root: effective_root,
        cade_files,
        all_watch_files,
        nix_store_paths,
        rollup,
    }))
}

fn reusable_cached_layer(
    cade: &Cade,
    dir: &str,
    token: &str,
    path: &Path,
) -> Result<Option<(CadeLayer, Vec<String>)>> {
    let Some(cached) = get_cached_layer(cade, dir, token)? else {
        return Ok(None);
    };
    if store_paths_all_present(&cached.nix_store_paths) {
        let layer = cached
            .activate()
            .with_context(|| format!("replaying entry actions for {}", path.display()))?;
        let store_paths = layer.nix_store_paths.clone();
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

pub(super) fn activation_env_with_snapshot(cade: &Cade) -> Result<(ActivationEnv, String, bool)> {
    let live = live_ambient_env();
    if let Some(session) = var(SESSION_VAR)
        .ok()
        .filter(|candidate| is_valid_session(candidate))
    {
        let baseline = read_snapshot(cade, &session).unwrap_or_else(|| live.clone());
        Ok((ActivationEnv { live, baseline }, session, false))
    } else {
        let session = new_session_id();
        gc_state(cade, None);
        write_snapshot(cade, &session, &live)?;
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

fn export_session(cade: &Cade) -> direnv_export::ExportSession {
    let snapshot = var(SESSION_VAR)
        .ok()
        .and_then(|session| read_snapshot(cade, &session));
    direnv_export::capture_session(snapshot)
}

pub fn export_env_delta(
    cade: &Cade,
    client_id: Option<&str>,
    owner_pid: Option<u32>,
) -> Result<EnvDelta> {
    let export = export_session(cade);
    if !direnv_mode().runs_shim() {
        return Ok(direnv_export::inactive_delta(export.previous));
    }
    let Some(root) = find_cade_root(&cade.cwd) else {
        return Ok(direnv_export::inactive_delta(export.previous));
    };

    let session = direnv_session_id(client_id, owner_pid)
        .unwrap_or_else(|| direnv_fallback_session_id(&root));
    let Some(plan) = maybe_activation_plan_for_root(cade, &root, &session)? else {
        if export.previous.is_some() {
            return Ok(direnv_export::inactive_delta(export.previous));
        }
        anyhow::bail!(
            "cade project is not allowed; run `cade allow` in {}",
            cade.cwd.display()
        );
    };
    root_nix_store_paths(cade, &session, &plan.nix_store_paths);
    refresh_session_holders(cade, &session, client_id, owner_pid);
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
    let delta = activation_env.delta(&plan.rollup);
    direnv_export::active_delta(
        delta,
        &activation_env.baseline,
        export.previous.as_ref(),
        metadata,
    )
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
    paths
        .iter()
        .all(|store_path| Path::new(store_path).exists())
}

#[cfg(test)]
mod tests {
    use super::store_paths_all_present;
    use std::{
        env::temp_dir,
        fs::{create_dir_all, remove_dir_all, write},
        process::id,
        slice::from_ref,
        thread::current,
    };

    #[test]
    fn store_paths_all_present_is_vacuously_true_when_empty() {
        assert!(store_paths_all_present(&[]));
    }

    #[test]
    fn store_paths_all_present_detects_a_missing_path() {
        let dir = temp_dir().join(format!(
            "cade-storepaths-{}-{}",
            id(),
            current().name().unwrap_or("test")
        ));
        create_dir_all(&dir).unwrap();
        let present_path = dir.join("present");
        write(&present_path, b"").unwrap();
        let present = present_path.to_string_lossy().to_string();
        let missing = dir.join("missing").to_string_lossy().to_string();

        assert!(store_paths_all_present(from_ref(&present)));
        assert!(!store_paths_all_present(&[present, missing]));

        let _ = remove_dir_all(&dir);
    }
}
