use super::{
    Announce, Cade, WatchState, announce_loaded, announce_unloaded, clear_disallowed_root_marker,
    mark_disallowed_root, shell_state::ShellState,
};
use crate::shells::ShellOutput;
use anyhow::Result;
use std::{collections::BTreeSet, path::Path};

impl Cade {
    pub fn do_reload(
        &mut self,
        shell: &dyn ShellOutput,
        client_id: Option<&str>,
        owner_pid: Option<u32>,
    ) -> Result<()> {
        let cwd = self.cwd.clone();
        let (active, disallowed_tip) = self.resolve_active(&cwd)?;
        let new_root = active.first().cloned();
        let new_set: BTreeSet<String> = active
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let shell_state = ShellState::from_env();

        if !shell_state.is_active() {
            if new_root.is_some() {
                self.do_activation(shell, Some(Announce::Loaded), client_id, owner_pid)?;
            }
            self.sync_disallowed_prompt(disallowed_tip.as_deref(), shell);
            return Ok(());
        }

        if let Some(session) = shell_state.valid_session() {
            self.refresh_session_holders(session, client_id, owner_pid);
        }

        let state = shell_state.watch_state();
        let old_set: BTreeSet<String> = state.map(WatchState::cade_path_set).unwrap_or_default();
        let old_root = state.map(WatchState::root_string);
        let files_stale = state.map(WatchState::files_changed).unwrap_or(true);

        if new_set == old_set && !files_stale {
            self.sync_disallowed_prompt(disallowed_tip.as_deref(), shell);
            return Ok(());
        }

        match &new_root {
            None => {
                self.do_restore(shell, true, true, client_id, owner_pid)?;
            }
            Some(new_root) => {
                let new_tip = new_root.to_string_lossy().to_string();
                let old_tip = old_root.as_deref();

                let unload_old_tip = old_tip.is_none_or(|t| !new_set.contains(t));
                let verb = if old_tip == Some(new_tip.as_str()) {
                    Some(Announce::Reloaded)
                } else if old_set.contains(&new_tip) {
                    None
                } else {
                    Some(Announce::Loaded)
                };

                self.do_restore(shell, false, unload_old_tip, client_id, owner_pid)?;
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
                self.do_activation(shell, verb, client_id, owner_pid)?;
            }
        }
        self.sync_disallowed_prompt(disallowed_tip.as_deref(), shell);
        Ok(())
    }

    fn sync_disallowed_prompt(&self, disallowed_tip: Option<&Path>, shell: &dyn ShellOutput) {
        match disallowed_tip {
            Some(tip) => mark_disallowed_root(tip, shell),
            None => clear_disallowed_root_marker(shell),
        }
    }
}
