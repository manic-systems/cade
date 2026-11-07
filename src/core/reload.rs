use std::{
    collections::BTreeSet,
    env::var,
    fs::create_dir_all,
    path::{
        Path,
        PathBuf,
    },
};

use misstep::{
    OptionExt as _,
    Result,
    ResultExt as _,
};

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
        participants::find_cade_root,
        permissions::resolve_active,
        restore::do_restore,
        sessions::{
            atomic_write,
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
            reload_marker,
        },
    },
    shells::ShellOutput,
};

pub fn request_reload(cade: &Cade) -> Result<()> {
    let root = find_cade_root(&cade.cwd)
        .context("no .cade or .envrc found in this directory or any parent")?;
    let (mut active, _) = resolve_active(cade, &cade.cwd)?;
    if !active.contains(&root) {
        active.push(root);
    }
    create_dir_all(cade.state_dir.join("reload")).context("create reload marker directory")?;
    let generation = new_session_id();
    for dir in active {
        atomic_write(&reload_marker(cade, &dir), generation.as_bytes())
            .context("request layer reload")?;
    }
    Ok(())
}

pub fn do_reload(
    cade: &Cade,
    shell: &dyn ShellOutput,
    client_id: Option<&str>,
    owner_pid: Option<u32>,
) -> Result<()> {
    let (active, disallowed_tip) = resolve_active(cade, &cade.cwd)?;
    let new_set = active
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    if !failed_activation_unchanged(shell, &new_set) {
        reload_current(
            cade,
            shell,
            active.first().map(PathBuf::as_path),
            &new_set,
            client_id,
            owner_pid,
        )?;
    }
    sync_disallowed_prompt(disallowed_tip.as_deref(), shell);
    Ok(())
}

fn failed_activation_unchanged(shell: &dyn ShellOutput, new_set: &BTreeSet<String>) -> bool {
    let Ok(failed_ref) = var(FAILED_WATCHES_VAR) else {
        return false;
    };
    let unchanged = load_watch_ref(&failed_ref).is_some_and(|mut failed| {
        failed.cade_path_set() == *new_set && failed.refresh() != WatchChange::Content
    });
    if !unchanged {
        print!("{}", shell.unset_env(FAILED_WATCHES_VAR));
    }
    unchanged
}

fn reload_current(
    cade: &Cade,
    shell: &dyn ShellOutput,
    new_root: Option<&Path>,
    new_set: &BTreeSet<String>,
    client_id: Option<&str>,
    owner_pid: Option<u32>,
) -> Result<()> {
    let mut shell_state = ShellState::from_env();
    if !shell_state.is_active() {
        return match new_root {
            Some(_) => {
                activate_or_mark_failed(cade, shell, Some(Announce::Loaded), client_id, owner_pid)
            },
            None => Ok(()),
        };
    }
    if let Some(session) = shell_state.valid_session() {
        refresh_session_holders(cade, session, client_id, owner_pid);
    }
    let mut state = shell_state.take_watch_state();
    let old_set = state
        .as_ref()
        .map(WatchState::cade_path_set)
        .unwrap_or_default();
    let old_root = state.as_ref().map(WatchState::root_string);
    let change = state
        .as_mut()
        .map_or(WatchChange::Content, WatchState::refresh);
    if *new_set == old_set && change != WatchChange::Content {
        return refresh_watch_metadata(cade, shell, &shell_state, state.as_ref(), change);
    }
    new_root.map_or_else(
        || {
            do_restore(cade, shell, true, true, client_id, owner_pid);
            Ok(())
        },
        |root| {
            let transition = LayerTransition {
                old_set: &old_set,
                new_set,
                old_tip: old_root.as_deref(),
                new_root: root,
            };
            let announcement = transition.restore(cade, shell, client_id, owner_pid);
            activate_or_mark_failed(cade, shell, announcement, client_id, owner_pid)
        },
    )
}

fn refresh_watch_metadata(
    cade: &Cade,
    shell: &dyn ShellOutput,
    shell_state: &ShellState,
    watches: Option<&WatchState>,
    change: WatchChange,
) -> Result<()> {
    if change == WatchChange::Metadata
        && let Some(session) = shell_state.valid_session()
        && let Some(current_watches) = watches
    {
        let watches_ref = persist_watch_state(cade, session, current_watches)?;
        print!("{}", shell.set_env(WATCHES_VAR, &watches_ref));
    }
    Ok(())
}

struct LayerTransition<'a> {
    old_set:  &'a BTreeSet<String>,
    new_set:  &'a BTreeSet<String>,
    old_tip:  Option<&'a str>,
    new_root: &'a Path,
}

impl LayerTransition<'_> {
    fn restore(
        &self,
        cade: &Cade,
        shell: &dyn ShellOutput,
        client_id: Option<&str>,
        owner_pid: Option<u32>,
    ) -> Option<Announce> {
        let unload_old_tip = self.old_tip.is_none_or(|tip| !self.new_set.contains(tip));
        do_restore(cade, shell, false, unload_old_tip, client_id, owner_pid);
        for dir in self.old_set.difference(self.new_set) {
            if Some(dir.as_str()) != self.old_tip {
                announce_unloaded(dir, 1);
            }
        }
        let new_tip = self.new_root.to_string_lossy();
        for dir in self.new_set.difference(self.old_set) {
            if dir != new_tip.as_ref() {
                announce_loaded(dir);
            }
        }
        if self.old_tip == Some(new_tip.as_ref()) {
            return Some(Announce::Reloaded);
        }
        if self.old_set.contains(new_tip.as_ref()) {
            return None;
        }
        Some(Announce::Loaded)
    }
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
