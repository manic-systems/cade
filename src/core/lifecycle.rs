use super::{
    Announce, Cade, WatchState, announce_loaded, announce_unloaded, build_watch_state,
    clear_disallowed_root_marker, files_changed, find_cade_root, is_valid_session, log_hook,
    log_key_list, mark_disallowed_root, participant_dirs, read_keylist,
};
use crate::{
    config,
    env_delta::is_shell_managed,
    shells::ShellOutput,
    types::{HookType, InnerHook},
};
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
        if new_session {
            print!("{}", shell.set_env("__CADE_SESSION", &session));
        }

        let delta = rollup.env_delta(&activation_env);
        print!("{}", delta.render_shell(shell));

        for hook in rollup.hooks() {
            if hook.kind == HookType::LoadPost {
                log_hook(hook);
                print!("{}", shell.emit_hook(&hook.content));
            }
        }

        let layer_paths: Vec<String> = plan
            .cade_files
            .iter()
            .map(|(p, _)| p.to_string_lossy().to_string())
            .collect();
        print!(
            "{}",
            shell.set_env("__CADE_LAYERS", &layer_paths.join("\x1F"))
        );
        print!(
            "{}",
            shell.set_env("__CADE_STATE_DIR", &self.state_dir.to_string_lossy())
        );
        if let Some(path) = config::current().path.as_deref() {
            print!(
                "{}",
                shell.set_env("__CADE_CONFIG_PATH", &path.to_string_lossy())
            );
        }

        let set_keys = rollup.set_keys();
        print!("{}", shell.set_env("__CADE_SET", &set_keys.join("\x1F")));
        print!(
            "{}",
            shell.set_env("__CADE_UNSET", &rollup.unset().join("\x1F"))
        );
        print!(
            "{}",
            shell.set_env("__CADE_PURE", if rollup.purified() { "1" } else { "0" })
        );

        let hooks_json = serde_json::to_string(rollup.hooks()).unwrap_or_default();
        print!("{}", shell.set_env("__CADE_HOOKS", &hooks_json));

        let watch_state = build_watch_state(&plan.root, layer_paths.clone(), &plan.all_watch_files);
        let watches_json = serde_json::to_string(&watch_state).unwrap_or_default();
        print!("{}", shell.set_env("__CADE_WATCHES", &watches_json));

        match announce {
            Some(announce) => spinner.success(&format!(
                "cade: {} {}{}.",
                announce.verb(),
                plan.root.display(),
                super::layer_count_suffix(layer_paths.len())
            )),
            None => spinner.done(),
        }
        log_key_list("set", set_keys);
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
        let layers = std::env::var("__CADE_LAYERS").ok();
        let session = std::env::var("__CADE_SESSION").ok();

        if layers.is_none() && session.is_none() && std::env::var("__CADE_SET").is_err() {
            return Ok(());
        }

        let prev_env: HashMap<String, String> = session
            .as_deref()
            .and_then(|s| self.read_snapshot(s))
            .unwrap_or_default();

        let set_keys = read_keylist("__CADE_SET");
        let unset_keys = read_keylist("__CADE_UNSET");
        let pure = std::env::var("__CADE_PURE")
            .map(|v| v == "1")
            .unwrap_or(false);

        let hooks: Vec<InnerHook> = std::env::var("__CADE_HOOKS")
            .ok()
            .and_then(|h| serde_json::from_str(&h).ok())
            .unwrap_or_default();

        if announce && let Some(layers) = &layers {
            let paths: Vec<&str> = layers.split('\x1F').filter(|s| !s.is_empty()).collect();
            if let Some(tip) = paths.last() {
                announce_unloaded(tip, paths.len());
            }
        }

        for hook in &hooks {
            if hook.kind == HookType::UnloadPre {
                log_hook(hook);
                print!("{}", shell.emit_hook(&hook.content));
            }
        }

        if pure {
            for (k, v) in &prev_env {
                if is_shell_managed(k) {
                    continue;
                }
                print!("{}", shell.set_env(k, v));
            }
            for k in &set_keys {
                if !prev_env.contains_key(k) && !is_shell_managed(k) {
                    print!("{}", shell.unset_env(k));
                }
            }
        } else {
            for k in &set_keys {
                if is_shell_managed(k) {
                    continue;
                }
                match prev_env.get(k) {
                    Some(prev_v) => print!("{}", shell.set_env(k, prev_v)),
                    None => print!("{}", shell.unset_env(k)),
                }
            }
        }

        for k in &unset_keys {
            if is_shell_managed(k) {
                continue;
            }
            if let Some(prev_v) = prev_env.get(k) {
                print!("{}", shell.set_env(k, prev_v));
            }
        }

        for var in [
            "__CADE_LAYERS",
            "__CADE_SET",
            "__CADE_UNSET",
            "__CADE_PURE",
            "__CADE_WATCHES",
            "__CADE_HOOKS",
            "__CADE_STATE_DIR",
            "__CADE_CONFIG_PATH",
        ] {
            print!("{}", shell.unset_env(var));
        }

        // nested shells share the snapshot
        if finalise {
            if let Some(session) = session.as_deref() {
                self.remove_current_session_holders(session, client_id, owner_pid);
            }
            self.gc_state(session.as_deref());
            print!("{}", shell.unset_env("__CADE_SESSION"));
        }

        for hook in &hooks {
            if hook.kind == HookType::UnloadPost {
                log_hook(hook);
                print!("{}", shell.emit_hook(&hook.content));
            }
        }

        log_key_list("restored", &set_keys);
        log_key_list("restored cleared", &unset_keys);

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
        let is_active = std::env::var("__CADE_LAYERS").is_ok();

        if !is_active {
            if new_root.is_some() {
                self.do_activation(shell, Some(Announce::Loaded), client_id, owner_pid)?;
                self.sync_disallowed_prompt(disallowed_tip.as_deref(), shell);
            } else {
                self.sync_disallowed_prompt(disallowed_tip.as_deref(), shell);
            }
            return Ok(());
        }

        if let Ok(session) = std::env::var("__CADE_SESSION")
            && is_valid_session(&session)
        {
            self.refresh_session_holders(&session, client_id, owner_pid);
        }

        let state = std::env::var("__CADE_WATCHES")
            .ok()
            .and_then(|w| serde_json::from_str::<WatchState>(&w).ok());
        let old_set: BTreeSet<String> = state
            .as_ref()
            .map(|s| s.cade_paths.iter().cloned().collect())
            .unwrap_or_default();
        let old_root = state.as_ref().map(|s| s.root.clone());
        let files_stale = state.as_ref().map(files_changed).unwrap_or(true);

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
        let active = std::env::var("__CADE_LAYERS").is_ok();

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

        println!("active:  {}", if active { "yes" } else { "no" });
        if active {
            let set = read_keylist("__CADE_SET");
            if !set.is_empty() {
                println!("set:     {}", set.join(", "));
            }
            let unset = read_keylist("__CADE_UNSET");
            if !unset.is_empty() {
                println!("cleared: {}", unset.join(", "));
            }
        }
        Ok(())
    }
}
