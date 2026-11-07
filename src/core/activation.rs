use std::{
    collections::BTreeMap,
    env::var,
    path::{
        Path,
        PathBuf,
    },
};

use misstep::{
    OptionExt as _,
    Result,
    ResultExt as _,
    report,
};
use serde::{
    Deserialize,
    Serialize,
};

use crate::{
    cade_file::ConfigSnapshot,
    config::direnv_mode,
    core::{
        Cade,
        DISALLOWED_REMINDER,
        cache::{
            get_cached_layer,
            store_cached_layer,
        },
        layer::load_single_layer,
        participants::find_cade_root,
        permissions::approved_chain,
        sessions::{
            direnv_fallback_session_id,
            direnv_session_id,
            gc_roots::{
                gc_state,
                refresh_session_holders,
                root_nix_store_paths,
            },
            is_valid_session,
            new_session_id,
            stable_hash_hex,
        },
        shell_state::SESSION_VAR,
        snapshot::{
            read_snapshot,
            write_snapshot,
        },
        watch::{
            WatchState,
            layer_watch,
        },
    },
    direnv_export,
    env::{
        delta::{
            EnvDelta,
            EnvDeltaInput,
            live_ambient_env,
        },
        rollup::{
            RollupResult,
            rollup_envs,
        },
    },
    envrc::{
        envrc_arg,
        envrc_uses_nix,
    },
    path_resolve::resolve_for_watch,
    types::{
        keyword::{
            Keyword,
            Loadable,
        },
        layer::CadeLayer,
    },
    verbosity::{
        self,
        Verbosity,
    },
};

#[derive(Serialize, Deserialize)]
pub(super) struct ActivationPlan {
    pub root:            PathBuf,
    pub config_file:     PathBuf,
    pub layer_paths:     Vec<PathBuf>,
    pub all_watch_files: Vec<PathBuf>,
    pub nix_store_paths: Vec<String>,
    pub rollup:          RollupResult,
}

pub(super) struct ActivationEnv {
    live:     BTreeMap<String, String>,
    baseline: BTreeMap<String, String>,
}

impl ActivationEnv {
    pub(super) fn delta(&self, rollup: &RollupResult) -> EnvDelta {
        EnvDelta::from_rollup(EnvDeltaInput {
            env:      rollup.env(),
            absorb:   rollup.absorb(),
            unset:    rollup.unset(),
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
        .ok_or_else(|| report!("{DISALLOWED_REMINDER}"))
}

fn maybe_activation_plan_for_root(
    cade: &Cade,
    root: &Path,
    session: &str,
) -> Result<Option<ActivationPlan>> {
    let approved = approved_chain(cade, root)?;
    let Some(tip) = approved.last() else {
        return Ok(None);
    };
    let root_path = tip.0.clone();
    let config_file = tip.0.join(tip.1.filename());
    let mut cade_files = Vec::with_capacity(approved.len());

    let mut cade_layers = Vec::new();
    let mut all_watch_files: Vec<PathBuf> = Vec::new();
    let mut nix_store_paths: Vec<String> = Vec::new();

    for (layer_count, (path, snapshot)) in approved.into_iter().enumerate() {
        let keywords = snapshot.keywords(&path)?;
        let (watch_files, token) = layer_watch(cade, &path, &keywords, &snapshot)?;
        all_watch_files.extend(watch_files);
        let entry = (path, keywords);
        let layer = activate_layer(cade, session, layer_count, &entry, &snapshot, &token)?;
        cade_files.push(entry);
        nix_store_paths.extend(layer.nix_store_paths.iter().cloned());
        cade_layers.push(layer);
    }

    let rollup = rollup_envs(cade_layers);

    Ok(Some(ActivationPlan {
        root: root_path,
        config_file,
        layer_paths: cade_files.into_iter().map(|entry| entry.0).collect(),
        all_watch_files,
        nix_store_paths,
        rollup,
    }))
}

fn activate_layer(
    cade: &Cade,
    session: &str,
    layer_count: usize,
    entry: &(PathBuf, Vec<Keyword>),
    snapshot: &ConfigSnapshot,
    token: &str,
) -> Result<CadeLayer> {
    let path = &entry.0;
    let dir = path.to_string_lossy();
    if let Some(layer) = reusable_cached_layer(cade, &dir, token, path)? {
        return Ok(layer);
    }
    verbosity::log(
        Verbosity::Trace,
        format_args!("cade: loading layer {}.", path.display()),
    );
    let (cached, layer) = load_single_layer(layer_count, path, &entry.1, snapshot, cade, session)?;
    store_cached_layer(cade, &dir, token, &cached)?;
    Ok(layer)
}

pub(super) fn chain_watch_state(cade: &Cade) -> Result<Option<WatchState>> {
    let Some(root) = find_cade_root(&cade.cwd) else {
        return Ok(None);
    };
    let cade_files = approved_chain(cade, &root)?;
    let Some(effective_root) = cade_files.last().map(|entry| entry.0.clone()) else {
        return Ok(None);
    };

    let mut watch_files = Vec::new();
    for layer_entry in &cade_files {
        let keywords = layer_entry.1.keywords(&layer_entry.0)?;
        watch_files.extend(layer_watch(cade, &layer_entry.0, &keywords, &layer_entry.1)?.0);
    }

    let layer_paths = cade_files.into_iter().map(|entry| entry.0).collect();
    Ok(Some(WatchState::capture(
        &effective_root,
        layer_paths,
        &watch_files,
    )))
}

pub(super) struct ChainKey {
    pub key:           String,
    pub runs_commands: bool,
}

pub(super) fn chain_key(cade: &Cade) -> Result<Option<ChainKey>> {
    let Some(root) = find_cade_root(&cade.cwd) else {
        return Ok(None);
    };
    let mut parts = Vec::new();
    let mut runs_commands = false;
    for (path, snapshot) in approved_chain(cade, &root)? {
        let keywords = snapshot.keywords(&path)?;
        let (_, token) = layer_watch(cade, &path, &keywords, &snapshot)?;
        parts.push(format!("{}\n{token}", path.display()));
        runs_commands |= keywords
            .iter()
            .any(|keyword| keyword_runs_command(&path, keyword));
    }
    Ok((!parts.is_empty()).then(|| {
        ChainKey {
            key: stable_hash_hex(&parts.join("\n")),
            runs_commands,
        }
    }))
}

fn keyword_runs_command(layer_dir: &Path, keyword: &Keyword) -> bool {
    match *keyword {
        Keyword::Call(_)
        | Keyword::Load(Loadable::Default | Loadable::Flake(_) | Loadable::Shell(_)) => true,
        Keyword::Load(Loadable::Envrc(ref file)) => {
            envrc_uses_nix(&resolve_for_watch(layer_dir, envrc_arg(file)))
        },
        Keyword::Load(Loadable::Env(_))
        | Keyword::Pure
        | Keyword::Disinherit
        | Keyword::Hook(_)
        | Keyword::Clear(_)
        | Keyword::Watch(_)
        | Keyword::Concat(_)
        | Keyword::Set(_) => false,
    }
}

fn reusable_cached_layer(
    cade: &Cade,
    dir: &str,
    token: &str,
    path: &Path,
) -> Result<Option<CadeLayer>> {
    let Some(cached) = get_cached_layer(cade, dir, token)? else {
        return Ok(None);
    };
    if store_paths_all_present(&cached.nix_store_paths) {
        let layer = cached
            .activate()
            .with_context(|| format!("replaying entry actions for {}", path.display()))?;
        verbosity::log(
            Verbosity::Trace,
            format_args!("cade: using cached layer {}.", path.display()),
        );
        Ok(Some(layer))
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
        misstep::bail!(
            "cade project is not allowed; run `cade allow` in {}",
            cade.cwd.display()
        );
    };
    root_nix_store_paths(cade, &session, &plan.nix_store_paths);
    refresh_session_holders(cade, &session, client_id, owner_pid);
    let activation_env = ActivationEnv {
        live:     export.live,
        baseline: export.baseline,
    };
    let metadata = direnv_export::ExportMetadata {
        root:    plan.root.to_string_lossy().to_string(),
        file:    plan.config_file.to_string_lossy().to_string(),
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

fn store_paths_all_present(paths: &[String]) -> bool {
    paths
        .iter()
        .all(|store_path| Path::new(store_path).exists())
}

#[cfg(test)]
mod tests {
    use std::{
        env::temp_dir,
        fs::{
            create_dir_all,
            remove_dir_all,
            write,
        },
        process::id,
        slice::from_ref,
        thread::current,
    };

    use super::store_paths_all_present;

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
