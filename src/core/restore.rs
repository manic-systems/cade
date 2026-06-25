use super::{Cade, announce_unloaded, log_hook, log_key_list, shell_state::ShellState};
use crate::{env::is_shell_managed, shells::ShellOutput, types::HookType};
use anyhow::Result;
use std::collections::HashMap;

impl Cade {
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
            restore_pure_env(shell, &shell_state, &prev_env);
        } else {
            restore_impure_env(shell, &shell_state, &prev_env);
        }
        restore_unset_vars(shell, &shell_state, &prev_env);

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
}

fn restore_pure_env(
    shell: &dyn ShellOutput,
    shell_state: &ShellState,
    prev_env: &HashMap<String, String>,
) {
    for (k, v) in prev_env {
        if !is_shell_managed(k) {
            print!("{}", shell.set_env(k, v));
        }
    }
    for k in shell_state.set_keys() {
        if !prev_env.contains_key(k) && !is_shell_managed(k) {
            print!("{}", shell.unset_env(k));
        }
    }
}

fn restore_impure_env(
    shell: &dyn ShellOutput,
    shell_state: &ShellState,
    prev_env: &HashMap<String, String>,
) {
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

fn restore_unset_vars(
    shell: &dyn ShellOutput,
    shell_state: &ShellState,
    prev_env: &HashMap<String, String>,
) {
    for k in shell_state.unset_keys() {
        if is_shell_managed(k) {
            continue;
        }
        if let Some(prev_v) = prev_env.get(k) {
            print!("{}", shell.set_env(k, prev_v));
        }
    }
}
