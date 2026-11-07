use std::{
    collections::BTreeMap,
    env::current_exe,
    fs::canonicalize,
    os::unix::process::CommandExt as _,
    path::Path,
    process::{
        Command,
        id,
    },
};

use misstep::{
    OptionExt as _,
    Result,
    ResultExt as _,
    bail,
};

use crate::{
    config,
    core::{
        Cade,
        activation::activation_plan_for_exec,
        sessions::{
            gc_roots::{
                gc_state,
                refresh_session_holders,
                remove_current_session_holders,
                root_nix_store_paths,
            },
            new_session_id,
        },
        shell_state::ShellState,
        snapshot::read_snapshot,
    },
    env::delta::{
        EnvDelta,
        EnvDeltaInput,
        is_shell_managed,
        live_ambient_env,
    },
    shells::ShellName,
    types::hook::HookType,
    verbosity::{
        self,
        Verbosity,
    },
};

pub fn do_exec(cade: &Cade, target: &Path, command: &[String]) -> Result<()> {
    if command.is_empty() {
        bail!("exec requires a command");
    }
    let directory = canonicalize(target).context("resolve exec directory")?;
    if !directory.is_dir() {
        bail!("exec target is not a directory: {}", directory.display());
    }
    let mut environment = restored_ambient_env(cade)?;
    environment.remove("BASH_ENV");
    environment.insert("PWD".to_owned(), directory.to_string_lossy().into_owned());
    let error = internal_exec_command(cade)?
        .args(command)
        .env_clear()
        .envs(environment)
        .current_dir(directory)
        .exec();
    Err(error).context("enter cade exec environment")?
}

// Restoring the map before re-exec makes expansion, subprocesses and Nix see
// the same baseline that the final command will inherit. Unrelated live edits
// survive, including newly set variables in a previously purified shell.
fn restored_ambient_env(cade: &Cade) -> Result<BTreeMap<String, String>> {
    let mut restored = live_ambient_env();
    let state = ShellState::from_env();
    let baseline = exec_baseline(cade, &state)?;
    if state.pure() {
        for (key, value) in baseline.iter().filter(|entry| !is_shell_managed(entry.0)) {
            restored.insert(key.clone(), value.clone());
        }
    }
    for key in state.set_keys() {
        if !is_shell_managed(key) {
            match baseline.get(key) {
                Some(value) => {
                    restored.insert(key.clone(), value.clone());
                },
                None => {
                    restored.remove(key);
                },
            }
        }
    }
    for key in state.unset_keys() {
        if !is_shell_managed(key)
            && let Some(value) = baseline.get(key)
        {
            restored.insert(key.clone(), value.clone());
        }
    }
    Ok(restored)
}

fn exec_baseline(cade: &Cade, state: &ShellState) -> Result<BTreeMap<String, String>> {
    if state.is_empty() && !state.pure() && state.unset_keys().is_empty() {
        return Ok(BTreeMap::new());
    }
    let session = state.valid_session().context(
        "cannot restore active cade environment: missing or invalid session; run exec from a \
         clean shell",
    )?;
    read_snapshot(cade, session).context(
        "cannot restore active cade environment: baseline snapshot is missing or unreadable; run \
         exec from a clean shell",
    )
}

fn internal_exec_command(cade: &Cade) -> Result<Command> {
    let mut child = Command::new(current_exe().context("resolve cade executable for exec")?);
    child.args(["--verbosity", match verbosity::current() {
        Verbosity::Quiet => "quiet",
        Verbosity::Normal => "normal",
        Verbosity::Vars => "vars",
        Verbosity::Trace => "trace",
    }]);
    if let Some(path) = config::current().path.as_ref() {
        child.arg("--config").arg(path);
    }
    child
        .arg("__exec-stage")
        .arg("--state-dir")
        .arg(canonicalize(&cade.state_dir).context("resolve exec state directory")?)
        .arg("--");
    Ok(child)
}

// The holder follows this PID across both exec boundaries. Successful exec
// never drops it; the existing collector reaps it once the command exits.
struct ExecSession<'cade> {
    cade:    &'cade Cade,
    session: String,
}

impl<'cade> ExecSession<'cade> {
    fn new(cade: &'cade Cade) -> Self {
        let session = new_session_id();
        refresh_session_holders(cade, &session, Some(""), Some(id()));
        Self { cade, session }
    }
}

impl Drop for ExecSession<'_> {
    fn drop(&mut self) {
        remove_current_session_holders(self.cade, &self.session, Some(""), Some(id()));
    }
}

pub fn do_exec_stage(cade: &Cade, command: &[String]) -> Result<()> {
    if command.is_empty() {
        bail!("exec requires a command");
    }
    gc_state(cade, None);
    let session = ExecSession::new(cade);
    let plan = activation_plan_for_exec(cade, &session.session)?;
    root_nix_store_paths(cade, &session.session, &plan.nix_store_paths);
    let live = live_ambient_env();
    let shell = ShellName::Bash.get_output();
    let mut script = String::new();
    for hook in plan.rollup.hooks() {
        if hook.kind == HookType::LoadPre {
            script.push_str(&hook.content);
            script.push('\n');
        }
    }
    let delta = EnvDelta::from_rollup(EnvDeltaInput {
        env:      plan.rollup.env(),
        absorb:   plan.rollup.absorb(),
        unset:    plan.rollup.unset(),
        purified: plan.rollup.purified(),
        live_env: &live,
        baseline: &live,
    });
    script.push_str(&delta.render_shell(shell.as_ref()));
    for hook in plan.rollup.hooks() {
        if hook.kind == HookType::LoadPost {
            script.push_str(&hook.content);
            script.push('\n');
        }
    }
    script.push_str("\nexec \"$@\"\n");
    let error = Command::new("/bin/sh")
        .args(["-c", &script, "cade exec"])
        .args(command)
        .env_remove("BASH_ENV")
        .exec();
    Err(error).context("run command in cade environment")?
}
