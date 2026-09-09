use std::collections::BTreeMap;

use crate::{
    core::{
        Cade,
        announce_unloaded,
        log_hook,
        log_key_list,
        sessions::gc_roots::{
            gc_state,
            remove_current_session_holders,
        },
        shell_state::ShellState,
        snapshot::read_snapshot,
    },
    env::delta::is_shell_managed,
    shells::ShellOutput,
    types::hook::HookType,
};

pub fn do_restore(
    cade: &Cade,
    shell: &dyn ShellOutput,
    finalise: bool,
    announce: bool,
    client_id: Option<&str>,
    owner_pid: Option<u32>,
) {
    let shell_state = ShellState::from_env();

    if shell_state.is_empty() {
        return;
    }

    let prev_env: BTreeMap<String, String> = shell_state
        .session()
        .and_then(|session| read_snapshot(cade, session))
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

    print!("{}", ShellState::render_clear(shell, finalise));

    if finalise {
        if let Some(session) = shell_state.session() {
            remove_current_session_holders(cade, session, client_id, owner_pid);
        }
        gc_state(cade, shell_state.session());
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
}

fn restore_pure_env(
    shell: &dyn ShellOutput,
    shell_state: &ShellState,
    prev_env: &BTreeMap<String, String>,
) {
    for (key, value) in prev_env {
        if !is_shell_managed(key) {
            print!("{}", shell.set_env(key, value));
        }
    }
    for key in shell_state.set_keys() {
        if !prev_env.contains_key(key) && !is_shell_managed(key) {
            print!("{}", shell.unset_env(key));
        }
    }
}

fn restore_impure_env(
    shell: &dyn ShellOutput,
    shell_state: &ShellState,
    prev_env: &BTreeMap<String, String>,
) {
    for key in shell_state.set_keys() {
        if is_shell_managed(key) {
            continue;
        }
        match prev_env.get(key) {
            Some(prev_value) => print!("{}", shell.set_env(key, prev_value)),
            None => print!("{}", shell.unset_env(key)),
        }
    }
}

fn restore_unset_vars(
    shell: &dyn ShellOutput,
    shell_state: &ShellState,
    prev_env: &BTreeMap<String, String>,
) {
    for key in shell_state.unset_keys() {
        if is_shell_managed(key) {
            continue;
        }
        if let Some(prev_value) = prev_env.get(key) {
            print!("{}", shell.set_env(key, prev_value));
        }
    }
}
