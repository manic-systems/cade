use super::{
    Announce, Cade, WatchState, clear_disallowed_root_marker, find_cade_root, log_hook,
    log_key_list,
};
use crate::{config, shells::ShellOutput, types::HookType};
use anyhow::Result;

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
        let shell_state = super::shell_state::ShellState::active(
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
}
