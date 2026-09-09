use crate::core::activation::activation_env_with_snapshot;
use crate::core::activation::activation_plan;
use crate::core::participants::find_cade_root;
use crate::core::sessions::gc_roots::refresh_session_holders;
use crate::core::sessions::gc_roots::root_nix_store_paths;
use crate::core::shell_state::ShellState;
use crate::core::watch::WatchState;
use crate::core::watch::persist_watch_state;
use crate::core::{
    Announce, Cade, clear_disallowed_root_marker, layer_count_suffix, log_hook, log_key_list,
};
use crate::{config, progress::start, shells::ShellOutput, types::hook::HookType};
use anyhow::Result;

pub fn do_activation(
    cade: &Cade,
    shell: &dyn ShellOutput,
    announce: Option<Announce>,
    client_id: Option<&str>,
    owner_pid: Option<u32>,
) -> Result<()> {
    let root_hint = find_cade_root(&cade.cwd).unwrap_or_else(|| cade.cwd.clone());
    let spinner = start(&root_hint.display().to_string());

    let (activation_env, session, new_session) = activation_env_with_snapshot(cade)?;
    let plan = activation_plan(cade, &session)?;
    refresh_session_holders(cade, &session, client_id, owner_pid);
    clear_disallowed_root_marker(shell);
    let rollup = &plan.rollup;

    for hook in rollup.hooks() {
        if hook.kind == HookType::LoadPre {
            log_hook(hook);
            print!("{}", shell.emit_hook(&hook.content));
        }
    }

    root_nix_store_paths(cade, &session, &plan.nix_store_paths);

    let delta = activation_env.delta(rollup);
    print!("{}", delta.render_shell(shell));

    for hook in rollup.hooks() {
        if hook.kind == HookType::LoadPost {
            log_hook(hook);
            print!("{}", shell.emit_hook(&hook.content));
        }
    }

    let layer_paths: Vec<_> = plan
        .cade_files
        .iter()
        .map(|layer| layer.0.clone())
        .collect();
    let set_keys: Vec<String> = rollup.set_keys().into_iter().map(str::to_owned).collect();
    let watch_state = WatchState::capture(&plan.root, layer_paths.clone(), &plan.all_watch_files);
    let watches_ref = persist_watch_state(cade, &session, &watch_state)?;
    let shell_state = ShellState::active(
        session,
        layer_paths.clone(),
        cade.state_dir.clone(),
        config::current().path.clone(),
        rollup,
        watches_ref,
    );
    print!("{}", shell_state.render_activation(shell, new_session));

    match announce {
        Some(detail) => spinner.success(&format!(
            "cade: {} {}{}.",
            detail.verb(),
            plan.root.display(),
            layer_count_suffix(layer_paths.len())
        )),
        None => spinner.done(),
    }
    log_key_list("set", &set_keys);
    log_key_list("cleared", rollup.unset());

    println!();
    Ok(())
}
