mod common;

use std::{
    env::var_os,
    fs::{
        File,
        FileTimes,
        read_dir,
        read_to_string,
    },
    io::{
        BufRead as _,
        BufReader,
    },
    os::unix::process::ExitStatusExt as _,
    path::PathBuf,
    process::{
        Child,
        Command,
        Stdio,
    },
    time::SystemTime,
};

use common::{
    Sandbox,
    stderr,
    stdout,
};

#[test]
fn exec_preserves_arguments_and_uses_target_directory() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "EXEC_VALUE=loaded\n");
    let child = sb.dir("child");
    sb.allow(&sb.root);
    let out = sb.run(
        &sb.root,
        &[
            "exec",
            "child",
            "--",
            "/bin/sh",
            "-c",
            "printf '%s\\n' \"$PWD\" \"$EXEC_VALUE\" \"$1\" \"$2\"",
            "sh",
            "a b",
            "$(false)",
        ],
        &[],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("{}\nloaded\na b\n$(false)\n", child.display())
    );
}

#[test]
fn exec_refuses_unapproved_environment() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "EXEC_VALUE=secret\n");
    let out = sb.run(&sb.root, &["exec", ".", "--", "/bin/echo", "ran"], &[]);
    assert!(!out.status.success());
    assert!(!stdout(&out).contains("ran"));
    assert!(stderr(&out).contains("allow"), "{}", stderr(&out));
}

#[test]
fn exec_applies_clear_and_pure() {
    let sb = Sandbox::new();
    sb.write(".cade", "pure\nload env\n");
    sb.write(".env", "EXEC_VALUE=loaded\n");
    sb.allow(&sb.root);
    let out = sb.run(
        &sb.root,
        &[
            "exec",
            ".",
            "--",
            "/bin/sh",
            "-c",
            "printf '%s:%s' \"${AMBIENT-unset}\" \"$EXEC_VALUE\"",
        ],
        &[("AMBIENT", "discard")],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "unset:loaded");
}

#[test]
fn exec_returns_command_exit_and_signal() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "EXEC_VALUE=loaded\n");
    sb.allow(&sb.root);
    let out = sb.run(
        &sb.root,
        &["exec", ".", "--", "/bin/sh", "-c", "exit 37"],
        &[],
    );
    assert_eq!(out.status.code(), Some(37_i32), "{}", stderr(&out));
    let signalled = sb.run(
        &sb.root,
        &["exec", ".", "--", "/bin/sh", "-c", "kill -TERM $$"],
        &[],
    );
    assert_eq!(
        signalled.status.signal(),
        Some(15_i32),
        "{}",
        stderr(&signalled)
    );
}

#[test]
fn exec_restores_parent_snapshot_before_loading() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "EXEC_VALUE=child\n");
    sb.allow(&sb.root);
    sb.write_snapshot(
        "exec-parent",
        "EXEC_VALUE=baseline\u{1f}PARENT_ONLY=original",
    );
    let out = sb.run(
        &sb.root,
        &[
            "exec",
            ".",
            "--",
            "/bin/sh",
            "-c",
            "printf '%s:%s:%s' \"$EXEC_VALUE\" \"$PARENT_ONLY\" \"${__CADE_SESSION-unset}\"",
        ],
        &[
            ("__CADE_SESSION", "exec-parent"),
            ("__CADE_SET", "EXEC_VALUE\u{1f}PARENT_ONLY"),
            ("EXEC_VALUE", "parent"),
            ("PARENT_ONLY", "modified"),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "child:original:unset");
}

#[test]
fn exec_applies_unset_and_load_hooks_without_emitting_shell_code() {
    let sb = Sandbox::new();
    sb.write(
        ".cade",
        "clear AMBIENT\nhook preload export PRE_HOOK=before\nhook load export POST_HOOK=after\n",
    );
    sb.allow(&sb.root);
    let out = sb.run(
        &sb.root,
        &[
            "exec",
            ".",
            "--",
            "/bin/sh",
            "-c",
            "printf '%s:%s:%s' \"${AMBIENT-unset}\" \"$PRE_HOOK\" \"$POST_HOOK\"",
        ],
        &[("AMBIENT", "discard")],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "unset:before:after");
}

#[test]
fn exec_evaluates_target_in_restored_environment_and_actual_directory() {
    let sb = Sandbox::new();
    sb.write(
        ".cade",
        "EXPANDED_SOURCE=${SOURCE}\nEXPANDED_PWD=${PWD}\ncall /bin/sh loader.sh\n",
    );
    sb.write(
        "loader.sh",
        "printf 'CALL_SOURCE=%s\\nCALL_CWD=%s\\nCALL_STATE=%s\\n' \"$SOURCE\" \"$PWD\" \
         \"${__CADE_SESSION-unset}\"\n",
    );
    let child = sb.dir("child");
    sb.allow(&sb.root);
    let parent_env = [
        ("SOURCE", "cached-parent"),
        ("PWD", sb.root.to_str().unwrap()),
        ("CADE_DIRENV", "full"),
    ];
    let warm = sb.run(&sb.root, &["export", "json"], &parent_env);
    assert!(warm.status.success(), "{}", stderr(&warm));
    let warm_delta: serde_json::Value = serde_json::from_str(&stdout(&warm)).unwrap();
    assert_eq!(warm_delta["CALL_SOURCE"], "cached-parent");
    sb.write_snapshot("exec-parent", "SOURCE=baseline");
    let out = sb.run(
        &sb.root,
        &[
            "exec",
            "child",
            "--",
            "/bin/sh",
            "-c",
            "printf '%s\\n' \"$EXPANDED_SOURCE\" \"$EXPANDED_PWD\" \"$CALL_SOURCE\" \"$CALL_CWD\" \
             \"$CALL_STATE\"",
        ],
        &[
            ("SOURCE", "parent"),
            ("PWD", "/wrong-parent-directory"),
            ("__CADE_SESSION", "exec-parent"),
            ("__CADE_SET", "SOURCE"),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!(
            "baseline\n{}\nbaseline\n{}\nunset\n",
            child.display(),
            sb.root.display()
        )
    );
    let interactive = sb.run(&sb.root, &["export", "json"], &parent_env);
    assert!(interactive.status.success(), "{}", stderr(&interactive));
    let interactive_delta: serde_json::Value = serde_json::from_str(&stdout(&interactive)).unwrap();
    assert_eq!(
        interactive_delta["CALL_SOURCE"], "cached-parent",
        "exec must not replace interactive cached evaluation"
    );
}

#[test]
fn exec_restores_pure_parent_and_preserves_unrelated_live_edits() {
    let sb = Sandbox::new();
    sb.write(".cade", "TARGET=${SOURCE}\n");
    sb.allow(&sb.root);
    sb.write_snapshot("exec-parent", "SOURCE=baseline\u{1f}CLEARED=restored");
    let out = sb.run(
        &sb.root,
        &[
            "exec",
            ".",
            "--",
            "/bin/sh",
            "-c",
            "printf '%s:%s:%s:%s' \"$TARGET\" \"$CLEARED\" \"$LIVE_EDIT\" \"${PARENT_ONLY-unset}\"",
        ],
        &[
            ("__CADE_SESSION", "exec-parent"),
            ("__CADE_PURE", "1"),
            ("__CADE_SET", "SOURCE\u{1f}PARENT_ONLY"),
            ("__CADE_UNSET", "CLEARED"),
            ("SOURCE", "parent"),
            ("PARENT_ONLY", "parent"),
            ("LIVE_EDIT", "keep"),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "baseline:restored:keep:unset");
}

struct ExecChild(Child);

impl Drop for ExecChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn running_exec(sb: &Sandbox) -> ExecChild {
    ExecChild(
        Command::new(env!("CARGO_BIN_EXE_cade"))
            .args([
                "exec",
                ".",
                "--",
                "/bin/sh",
                "-c",
                "printf '%s\\n' \"$$\"; exec sleep 30",
            ])
            .current_dir(&sb.root)
            .env_clear()
            .env("XDG_STATE_HOME", &sb.state)
            .env("HOME", &sb.state)
            .env("PATH", var_os("PATH").unwrap_or_default())
            .env("CADE_SHELL_GC_ROOT_TTL_SECONDS", "1")
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn cade exec"),
    )
}

fn command_pid(child: &mut ExecChild) -> u32 {
    let mut line = String::new();
    BufReader::new(child.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    line.trim().parse().expect("command prints its PID")
}

fn session_directory(sb: &Sandbox) -> PathBuf {
    let sessions = read_dir(sb.state.join("cade/gcroots/shells"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(sessions.len(), 1, "one private exec session");
    sessions.into_iter().next().unwrap()
}

#[test]
fn exec_replaces_cade_and_supervisor_sigterm_reaches_command() {
    let sb = Sandbox::new();
    sb.write(".cade", "VALUE=loaded\n");
    sb.allow(&sb.root);
    let mut child = running_exec(&sb);
    let pid = command_pid(&mut child);
    assert_eq!(
        pid,
        child.0.id(),
        "cade must not remain as a waiting wrapper"
    );
    let holders = read_dir(session_directory(&sb).join("holders"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(holders.len(), 1);
    let holder: serde_json::Value =
        serde_json::from_str(&read_to_string(&holders[0]).unwrap()).unwrap();
    assert_eq!(holder["pid"], pid);
    assert!(
        Command::new("/bin/sh")
            .args(["-c", "kill -TERM \"$1\"", "sh", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(child.0.wait().unwrap().signal(), Some(15_i32));
}

#[test]
fn exec_collects_dead_command_sessions_on_next_entry() {
    let sb = Sandbox::new();
    sb.write(".cade", "VALUE=loaded\n");
    sb.allow(&sb.root);
    let mut child = running_exec(&sb);
    command_pid(&mut child);
    let session = session_directory(&sb);
    let session_name = session.file_name().unwrap().to_str().unwrap();
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    sb.write_snapshot(session_name, "VALUE=old");
    let snapshot = sb
        .state
        .join("cade/snapshots")
        .join(format!("{session_name}.env"));
    let old = FileTimes::new().set_modified(SystemTime::UNIX_EPOCH);
    File::open(session.join(".last-used"))
        .unwrap()
        .set_times(old)
        .unwrap();
    File::open(&snapshot).unwrap().set_times(old).unwrap();
    let out = sb.run(&sb.root, &["exec", ".", "--", "/bin/sh", "-c", ":"], &[(
        "CADE_SHELL_GC_ROOT_TTL_SECONDS",
        "1",
    )]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!session.exists(), "dead command's roots must be collected");
    assert!(
        !snapshot.exists(),
        "dead command's snapshot must be collected"
    );
}

#[test]
fn exec_preserves_explicit_config_and_state_without_inherited_shell_startup() {
    let sb = Sandbox::new();
    sb.write(".cade", "VALUE=loaded\n");
    sb.write(".envrc", "echo unexpected-envrc-output; exit 73\n");
    sb.write("config.toml", "direnv = 'none'\n");
    sb.write("startup.sh", "echo unexpected-startup-output\n");
    sb.dir("child");
    let allowed = sb.run(&sb.root, &["--config", "config.toml", "allow"], &[]);
    assert!(allowed.status.success(), "{}", stderr(&allowed));
    let active_state = sb.state.join("cade");
    let startup = sb.root.join("startup.sh");
    let other_state = sb.dir("other-state");
    let out = sb.run(
        &sb.root,
        &[
            "--config",
            "config.toml",
            "--verbosity",
            "quiet",
            "exec",
            "child",
            "--",
            "/bin/sh",
            "-c",
            "printf '%s:%s' \"$VALUE\" \"${BASH_ENV-unset}\"",
        ],
        &[
            ("XDG_STATE_HOME", other_state.to_str().unwrap()),
            ("__CADE_STATE_DIR", active_state.to_str().unwrap()),
            ("BASH_ENV", startup.to_str().unwrap()),
            ("CADE_VERBOSITY", "trace"),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "loaded:unset");
    assert_eq!(stderr(&out), "");
}

#[test]
fn exec_refuses_active_parent_without_usable_baseline() {
    let sb = Sandbox::new();
    sb.write(".cade", "VALUE=loaded\n");
    sb.allow(&sb.root);
    for activation in [
        &[("__CADE_SET", "SOURCE")][..],
        &[("__CADE_SESSION", "exec-missing"), ("__CADE_SET", "SOURCE")][..],
        &[("__CADE_SESSION", "../invalid"), ("__CADE_SET", "SOURCE")][..],
    ] {
        let out = sb.run(
            &sb.root,
            &["exec", ".", "--", "/bin/echo", "ran"],
            activation,
        );
        assert!(!out.status.success());
        assert_eq!(stdout(&out), "");
        assert!(
            stderr(&out).contains("cannot restore active cade environment"),
            "{}",
            stderr(&out)
        );
        assert!(stderr(&out).contains("clean shell"), "{}", stderr(&out));
    }
}
