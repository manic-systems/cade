use crate::core::Announce;
use crate::core::Cade;
use crate::core::announce_loaded;
use crate::core::announce_unloaded;
use crate::core::clear_disallowed_root_marker;
use crate::core::enter::do_activation;
use crate::core::mark_disallowed_root;
use crate::core::permissions::resolve_active;
use crate::core::restore::do_restore;
use crate::core::sessions::gc_roots::refresh_session_holders;
use crate::core::shell_state::ShellState;
use crate::core::shell_state::WATCHES_VAR;
use crate::core::watch::WatchChange;
use crate::core::watch::WatchState;
use crate::core::watch::persist_watch_state;
use crate::shells::ShellOutput;
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::Path;

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

    if !shell_state.is_active() {
        if new_root.is_some() {
            do_activation(cade, shell, Some(Announce::Loaded), client_id, owner_pid)?;
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
        }
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
            do_activation(cade, shell, verb, client_id, owner_pid)?;
        }
    }
    sync_disallowed_prompt(disallowed_tip.as_deref(), shell);
    Ok(())
}

fn sync_disallowed_prompt(disallowed_tip: Option<&Path>, shell: &dyn ShellOutput) {
    match disallowed_tip {
        Some(tip) => mark_disallowed_root(tip, shell),
        None => clear_disallowed_root_marker(shell),
    }
}
