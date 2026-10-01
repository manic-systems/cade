use std::{
    collections::BTreeSet,
    env::var,
    path::Path,
};

use anyhow::Result;

use crate::{
    core::{
        Announce,
        Cade,
        activation::chain_watch_state,
        announce_loaded,
        announce_unloaded,
        clear_disallowed_root_marker,
        enter::do_activation,
        mark_disallowed_root,
        permissions::resolve_active,
        restore::do_restore,
        sessions::{
            gc_roots::refresh_session_holders,
            is_valid_session,
            new_session_id,
        },
        shell_state::{
            FAILED_WATCHES_VAR,
            SESSION_VAR,
            ShellState,
            WATCHES_VAR,
        },
        watch::{
            WatchChange,
            WatchState,
            load_watch_ref,
            persist_watch_state,
        },
    },
    shells::ShellOutput,
};

pub fn do_reload(
    cade: &Cade,
    shell: &dyn ShellOutput,
    client_id: Option<&str>,
    owner_pid: Option<u32>,
) -> Result<()> {
    let (active, disallowed_tip) = resolve_active(cade, &cade.cwd)?;
    let new_root = active.first().cloned();
    let new_set: BTreeSet<String> = active
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect();
    let mut shell_state = ShellState::from_env();

    if let Ok(failed_ref) = var(FAILED_WATCHES_VAR) {
        let unchanged = load_watch_ref(&failed_ref).is_some_and(|mut failed| {
            failed.cade_path_set() == new_set && failed.refresh() != WatchChange::Content
        });
        if unchanged {
            sync_disallowed_prompt(disallowed_tip.as_deref(), shell);
            return Ok(());
        }
        print!("{}", shell.unset_env(FAILED_WATCHES_VAR));
    }

    if !shell_state.is_active() {
        if new_root.is_some() {
            activate_or_mark_failed(cade, shell, Some(Announce::Loaded), client_id, owner_pid)?;
        }
        sync_disallowed_prompt(disallowed_tip.as_deref(), shell);
        return Ok(());
    }

    if let Some(session) = shell_state.valid_session() {
        refresh_session_holders(cade, session, client_id, owner_pid);
    }

    let mut state = shell_state.take_watch_state();
    let old_set: BTreeSet<String> = state
        .as_ref()
        .map(WatchState::cade_path_set)
        .unwrap_or_default();
    let old_root = state.as_ref().map(WatchState::root_string);
    let change = state
        .as_mut()
        .map_or(WatchChange::Content, WatchState::refresh);

    if new_set == old_set && change != WatchChange::Content {
        if change == WatchChange::Metadata
            && let Some(session) = shell_state.valid_session()
            && let Some(watches) = state.as_ref()
        {
            let watches_ref = persist_watch_state(cade, session, watches)?;
            print!("{}", shell.set_env(WATCHES_VAR, &watches_ref));
        }
        sync_disallowed_prompt(disallowed_tip.as_deref(), shell);
        return Ok(());
    }

    match new_root.as_ref() {
        None => {
            do_restore(cade, shell, true, true, client_id, owner_pid);
        },
        Some(new_root_path) => {
            let new_tip = new_root_path.to_string_lossy().to_string();
            let old_tip = old_root.as_deref();

            let unload_old_tip = old_tip.is_none_or(|tip| !new_set.contains(tip));
            let verb = if old_tip == Some(new_tip.as_str()) {
                Some(Announce::Reloaded)
            } else if old_set.contains(&new_tip) {
                None
            } else {
                Some(Announce::Loaded)
            };

            do_restore(cade, shell, false, unload_old_tip, client_id, owner_pid);
            for dir in old_set.difference(&new_set) {
                if Some(dir.as_str()) != old_tip {
                    announce_unloaded(dir, 1);
                }
            }
            for dir in new_set.difference(&old_set) {
                if *dir != new_tip {
                    announce_loaded(dir);
                }
            }
            activate_or_mark_failed(cade, shell, verb, client_id, owner_pid)?;
        },
    }
    sync_disallowed_prompt(disallowed_tip.as_deref(), shell);
    Ok(())
}

fn activate_or_mark_failed(
    cade: &Cade,
    shell: &dyn ShellOutput,
    announce: Option<Announce>,
    client_id: Option<&str>,
    owner_pid: Option<u32>,
) -> Result<()> {
    let error = match do_activation(cade, shell, announce, client_id, owner_pid) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };

    let session = var(SESSION_VAR)
        .ok()
        .filter(|session| is_valid_session(session))
        .unwrap_or_else(new_session_id);
    if let Ok(Some(watches)) = chain_watch_state(cade)
        && let Ok(watches_ref) = persist_watch_state(cade, &session, &watches)
    {
        print!("{}", shell.set_env(FAILED_WATCHES_VAR, &watches_ref));
    }
    Err(error)
}

fn sync_disallowed_prompt(disallowed_tip: Option<&Path>, shell: &dyn ShellOutput) {
    match disallowed_tip {
        Some(tip) => mark_disallowed_root(tip, shell),
        None => clear_disallowed_root_marker(shell),
    }
}
