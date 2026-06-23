use super::{
    Announce, Cade, WatchState, announce_loaded, announce_unloaded, clear_disallowed_root_marker,
    find_cade_root, log_hook, log_key_list, mark_disallowed_root, participant_dirs,
    shell_state::ShellState,
};
use crate::{config, env::is_shell_managed, shells::ShellOutput, types::HookType};
use anyhow::Result;
use std::{collections::BTreeSet, collections::HashMap, path::Path};

impl Cade {
    pub fn do_activation(
        &mut self,
        shell: &dyn ShellOutput,
        announce: Option<Announce>,
        client_id: Option<&str>,
        owner_pid: Option<u32>,
    ) -> Result<()> {
        let root_hint = find_cade_root(&self.cwd).unwrap_or_else(|| self.cwd.clone());
        let spinner = crate::progress::start(&root_hint.display().to_string());

        let (activation_env, session, new_session) = self.activation_env_with_snapshot()?;
        let plan = self.activation_plan(Some(&session))?;
        self.refresh_session_holders(&session, client_id, owner_pid);
        clear_disallowed_root_marker(shell);
        let rollup = &plan.rollup;

        for hook in rollup.hooks() {
            if hook.kind == HookType::LoadPre {
                log_hook(hook);
                print!("{}", shell.emit_hook(&hook.content));
            }
        }

        self.root_nix_store_paths(&session, &plan.nix_store_paths);

        let delta = rollup.env_delta(&activation_env);
        print!("{}", delta.render_shell(shell));

        for hook in rollup.hooks() {
            if hook.kind == HookType::LoadPost {
                log_hook(hook);
                print!("{}", shell.emit_hook(&hook.content));
            }
        }

        let layer_paths: Vec<_> = plan.cade_files.iter().map(|(p, _)| p.clone()).collect();
        let set_keys: Vec<String> = rollup.set_keys().into_iter().map(str::to_string).collect();
        let shell_state = ShellState::active(
            session.clone(),
            layer_paths.clone(),
            self.state_dir.clone(),
            config::current().path.clone(),
            set_keys.clone(),
            rollup.unset().to_vec(),
            rollup.purified(),
            rollup.hooks().to_vec(),
            WatchState::capture(&plan.root, layer_paths.clone(), &plan.all_watch_files),
        );
        print!("{}", shell_state.render_activation(shell, new_session));

        match announce {
            Some(announce) => spinner.success(&format!(
                "cade: {} {}{}.",
                announce.verb(),
                plan.root.display(),
                super::layer_count_suffix(layer_paths.len())
            )),
            None => spinner.done(),
        }
        log_key_list("set", &set_keys);
        log_key_list("cleared", rollup.unset());

        println!();
        Ok(())
    }

    pub fn do_restore(
        &mut self,
        shell: &dyn ShellOutput,
        finalise: bool,
        announce: bool,
        client_id: Option<&str>,
        owner_pid: Option<u32>,
    ) -> Result<()> {
        let shell_state = ShellState::from_env();

        if shell_state.is_empty() {
            return Ok(());
        }

        let prev_env: HashMap<String, String> = shell_state
            .session()
            .and_then(|s| self.read_snapshot(s))
            .unwrap_or_default();

        if announce && let Some((tip, count)) = shell_state.unload_summary() {
            announce_unloaded(&tip, count);
        }

        for hook in shell_state.hooks() {
            if hook.kind == HookType::UnloadPre {
                log_hook(hook);
                print!("{}", shell.emit_hook(&hook.content));
            }
        }

        if shell_state.pure() {
            for (k, v) in &prev_env {
                if is_shell_managed(k) {
                    continue;
                }
                print!("{}", shell.set_env(k, v));
            }
            for k in shell_state.set_keys() {
                if !prev_env.contains_key(k) && !is_shell_managed(k) {
                    print!("{}", shell.unset_env(k));
                }
            }
        } else {
            for k in shell_state.set_keys() {
                if is_shell_managed(k) {
                    continue;
                }
                match prev_env.get(k) {
                    Some(prev_v) => print!("{}", shell.set_env(k, prev_v)),
                    None => print!("{}", shell.unset_env(k)),
                }
            }
        }

        for k in shell_state.unset_keys() {
            if is_shell_managed(k) {
                continue;
            }
            if let Some(prev_v) = prev_env.get(k) {
                print!("{}", shell.set_env(k, prev_v));
            }
        }

        print!("{}", shell_state.render_clear(shell, finalise));

        if finalise {
            if let Some(session) = shell_state.session() {
                self.remove_current_session_holders(session, client_id, owner_pid);
            }
            self.gc_state(shell_state.session());
        }

        for hook in shell_state.hooks() {
            if hook.kind == HookType::UnloadPost {
                log_hook(hook);
                print!("{}", shell.emit_hook(&hook.content));
            }
        }

        log_key_list("restored", shell_state.set_keys());
        log_key_list("restored cleared", shell_state.unset_keys());

        println!();
        Ok(())
    }

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
                self.sync_disallowed_prompt(disallowed_tip.as_deref(), shell);
            } else {
                self.sync_disallowed_prompt(disallowed_tip.as_deref(), shell);
            }
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
                // old tip unload and new tip verb are independent
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

    pub fn do_status(&mut self) -> Result<()> {
        let root = find_cade_root(&self.cwd);
        let shell_state = ShellState::from_env();

        println!("cwd:     {}", self.cwd.display());
        match &root {
            Some(r) => {
                println!("root:    {}", r.display());
                println!("layers (inner \u{2192} outer):");
                let mut capped = false;
                for dir in participant_dirs(r) {
                    let allowed = self.get_permission(&dir)?;
                    if !allowed {
                        capped = true;
                    }
                    let mark = if !allowed {
                        "not allowed  (run 'cade allow' here)"
                    } else if capped {
                        "allowed, but excluded (a lower layer is not allowed)"
                    } else {
                        "allowed, composed"
                    };
                    println!("  {}  [{mark}]", dir.display());
                }
            }
            None => println!("root:    none (not in a cade project)"),
        }

        println!(
            "active:  {}",
            if shell_state.is_active() { "yes" } else { "no" }
        );
        if shell_state.is_active() {
            if !shell_state.set_keys().is_empty() {
                println!("set:     {}", shell_state.set_keys().join(", "));
            }
            if !shell_state.unset_keys().is_empty() {
                println!("cleared: {}", shell_state.unset_keys().join(", "));
            }
        }
        Ok(())
    }
}
