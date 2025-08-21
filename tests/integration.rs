//! End-to-end tests that drive the real `cade` binary against temp directory
//! trees, asserting on the shell statements it emits. Each test gets an
//! isolated state directory (its own permission/cache DB) via XDG_STATE_HOME.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_cade");

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// An isolated sandbox: a project tree plus a private cade state directory.
struct Sandbox {
    root: PathBuf,
    state: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("cade-it-{}-{}", std::process::id(), id));
        let root = base.join("project");
        let state = base.join("state");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        Sandbox { root, state }
    }

    fn write(&self, rel: &str, contents: &str) {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn dir(&self, rel: &str) -> PathBuf {
        let p = self.root.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Write a pre-activation snapshot for `session` (as cade stores it under
    /// the state dir), so restore tests can simulate an active session.
    fn write_snapshot(&self, session: &str, contents: &str) {
        let dir = self.state.join("cade").join("snapshots");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{session}.env")), contents).unwrap();
    }

    /// Run `cade <args>` in `cwd` with an isolated, mostly-empty environment.
    fn run(&self, cwd: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .current_dir(cwd)
            .env_clear()
            .env("XDG_STATE_HOME", &self.state)
            .env("HOME", &self.state);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        cmd.output().expect("run cade")
    }

    fn allow(&self, cwd: &Path) {
        let out = self.run(cwd, &["allow"], &[]);
        assert!(out.status.success(), "allow failed: {:?}", out);
    }

    fn enter(&self, cwd: &Path, extra_env: &[(&str, &str)]) -> Output {
        self.run(cwd, &["enter", "--shell", "bash"], extra_env)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Some(base) = self.root.parent() {
            std::fs::remove_dir_all(base).ok();
        }
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn nested_layers_compose_child_first() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "A=1\nPATH=/parent/bin\n");
    let sub = sb.dir("sub");
    sb.write("sub/.cade", "load env\n");
    sb.write("sub/.env", "B=2\nPATH=/child/bin\n");

    // each layer must be explicitly allowed for it to compose
    sb.allow(&sb.root);
    sb.allow(&sub);
    let out = sb.enter(&sub, &[]);
    assert!(out.status.success(), "enter failed: {:?}", out);
    let s = stdout(&out);
    // scalars from each layer present
    assert!(s.contains("export A='1';"), "missing A: {s}");
    assert!(s.contains("export B='2';"), "missing B: {s}");
    // PATH is path-like: inner layer prepends (child : parent), no ambient here
    assert!(
        s.contains("export PATH='/child/bin:/parent/bin'"),
        "PATH not child-first: {s}"
    );
}

#[test]
fn activation_requires_permission() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "A=1\n");

    let out = sb.enter(&sb.root, &[]);
    assert!(!out.status.success(), "should fail without permission");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not permitted"), "unexpected stderr: {err}");
}

#[test]
fn activates_from_descendant_without_own_cade() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "A=1\n");
    let deep = sb.dir("a/b/c");

    // allow from the descendant: targets the innermost .cade ancestor (root)
    sb.allow(&deep);
    let out = sb.enter(&deep, &[]);
    assert!(out.status.success(), "enter failed: {:?}", out);
    assert!(stdout(&out).contains("export A='1';"));
}

#[test]
fn pure_discards_ambient_but_keeps_inherited_layers() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "INHERITED=1\n");
    let sub = sb.dir("sub");
    sb.write("sub/.cade", "pure\nload env\n");
    sb.write("sub/.env", "CHILD=2\n");

    sb.allow(&sb.root);
    sb.allow(&sub);
    let out = sb.enter(&sub, &[("AMBIENT_TEST", "zzz")]);
    assert!(out.status.success(), "enter failed: {:?}", out);
    let s = stdout(&out);
    // ambient var is purged
    assert!(s.contains("unset AMBIENT_TEST;"), "ambient not purged: {s}");
    // inherited parent-layer var survives, child layer applied
    assert!(
        s.contains("export INHERITED='1';"),
        "inherited dropped: {s}"
    );
    assert!(s.contains("export CHILD='2';"), "child missing: {s}");
    // shell-managed vars are never purged
    assert!(!s.contains("unset PWD;"), "must not purge PWD: {s}");
}

#[test]
fn hostile_env_value_is_single_quoted_and_inert() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "EVIL=$(touch /tmp/cade_pwned)\n");

    sb.allow(&sb.root);
    let out = sb.enter(&sb.root, &[]);
    assert!(out.status.success(), "enter failed: {:?}", out);
    let s = stdout(&out);
    assert!(
        s.contains("export EVIL='$(touch /tmp/cade_pwned)';"),
        "value not single-quoted: {s}"
    );
    // never the injectable double-quoted form
    assert!(!s.contains("EVIL=\""), "double-quoted form present: {s}");
}

#[test]
fn restore_reverts_only_cade_keys_and_leaves_pwd_alone() {
    let sb = Sandbox::new();
    // Simulate an active session: A was overridden (had "old"), B was added.
    sb.write_snapshot("s1", "A=old");
    let out = sb.run(
        &sb.root,
        &["exit", "--shell", "bash"],
        &[
            ("__CADE_SESSION", "s1"),
            ("__CADE_SET", "A\u{1f}B"),
            ("__CADE_UNSET", ""),
            ("__CADE_PURE", "0"),
            ("__CADE_HOOKS", "[]"),
            ("__CADE_LAYERS", "x"),
            ("A", "new"),
            ("B", "added"),
            ("PWD", "/somewhere/else"),
        ],
    );
    assert!(out.status.success(), "exit failed: {:?}", out);
    let s = stdout(&out);
    assert!(s.contains("export A='old';"), "A not restored: {s}");
    assert!(s.contains("unset B;"), "B not unset: {s}");
    // PWD is shell-managed: must never be restored to a stale value
    assert!(!s.contains("PWD"), "restore touched PWD: {s}");
    // a full exit ends the session
    assert!(s.contains("unset __CADE_SESSION;"));
}

#[test]
fn status_reports_permission_and_layers() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "A=1\n");

    let before = sb.run(&sb.root, &["status"], &[]);
    assert!(before.status.success(), "{:?}", before);
    assert!(
        stdout(&before).contains("not allowed"),
        "{}",
        stdout(&before)
    );

    sb.allow(&sb.root);
    let after = sb.run(&sb.root, &["status"], &[]);
    let s = stdout(&after);
    assert!(s.contains("allowed, composed"), "{s}");
    assert!(s.contains("layers"), "{s}");
    assert!(s.contains("active:  no"), "{s}");
}

#[test]
fn first_activation_emits_session_id_not_an_env_blob() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "A=1\n");
    sb.allow(&sb.root);

    let out = sb.enter(&sb.root, &[("SOMESECRET", "shh")]);
    assert!(out.status.success(), "{:?}", out);
    let s = stdout(&out);
    // a small session id, not the old exported full-env snapshot
    assert!(s.contains("export __CADE_SESSION="), "no session id: {s}");
    assert!(
        !s.contains("__CADE_PREV"),
        "should not emit the env blob: {s}"
    );
    // the ambient snapshot lives in a file, never echoed into the shell env
    assert!(
        !s.contains("SOMESECRET"),
        "ambient must not be duplicated into the env: {s}"
    );
}

#[test]
fn nested_shells_share_session_without_corrupting_restore() {
    let sb = Sandbox::new();
    // Parent and child shells share an inherited __CADE_SESSION + snapshot.
    sb.write_snapshot("shared", "PATH=/orig");

    let active_env = [
        ("__CADE_SESSION", "shared"),
        ("__CADE_SET", "PATH"),
        ("__CADE_UNSET", ""),
        ("__CADE_PURE", "0"),
        ("__CADE_HOOKS", "[]"),
        ("__CADE_LAYERS", "x"),
        ("PATH", "/layer:/orig"),
    ];

    // child shell tears down first
    let child = sb.run(&sb.root, &["exit", "--shell", "bash"], &active_env);
    assert!(child.status.success(), "{:?}", child);
    assert!(
        stdout(&child).contains("export PATH='/orig';"),
        "child restore: {}",
        stdout(&child)
    );

    // parent shell tears down later; the shared snapshot must still be intact
    let parent = sb.run(&sb.root, &["exit", "--shell", "bash"], &active_env);
    assert!(parent.status.success(), "{:?}", parent);
    assert!(
        stdout(&parent).contains("export PATH='/orig';"),
        "parent restore must still work after child teardown: {}",
        stdout(&parent)
    );
}

#[test]
fn untrusted_ancestor_layer_is_not_auto_activated() {
    let sb = Sandbox::new();
    // allow the tip BEFORE any ancestor .cade exists
    sb.write("proj/.cade", "load env\n");
    sb.write("proj/.env", "A=1\n");
    let proj = sb.dir("proj");
    sb.allow(&proj);

    // attacker later drops a .cade at the (never-allowed) parent
    sb.write(".cade", "hook load echo PWNED\n");

    // activating at the parent is blocked (the parent tip is not approved)
    let at_parent = sb.enter(&sb.root, &[]);
    assert!(
        !at_parent.status.success(),
        "untrusted ancestor must block: {:?}",
        at_parent
    );
    // the still-allowed tip activates, but the approved run caps below the
    // untrusted parent: its layer (and PWNED hook) is excluded, not run
    let at_tip = sb.enter(&proj, &[]);
    assert!(
        at_tip.status.success(),
        "tip should still activate: {:?}",
        at_tip
    );
    assert!(
        !stdout(&at_tip).contains("PWNED"),
        "untrusted ancestor layer must not be composed: {}",
        stdout(&at_tip)
    );
    assert!(stdout(&at_tip).contains("export A='1';"));
}

#[test]
fn layer_cannot_set_cade_internal_or_shell_managed_vars() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(
        ".env",
        "__CADE_SESSION=../../evil\n__CADE_LAYERS=x\nPWD=/evil\nSHLVL=99\nGOOD=ok\n",
    );
    sb.allow(&sb.root);

    let out = sb.enter(&sb.root, &[]);
    assert!(out.status.success(), "{:?}", out);
    let s = stdout(&out);
    assert!(s.contains("export GOOD='ok';"), "{s}");
    // reserved keys must never be taken from a layer
    assert!(!s.contains("evil"), "session/traversal value leaked: {s}");
    assert!(!s.contains("export PWD="), "PWD must not be layer-set: {s}");
    assert!(
        !s.contains("export SHLVL="),
        "SHLVL must not be layer-set: {s}"
    );
    assert!(
        !s.contains("export __CADE_LAYERS='x';"),
        "__CADE_LAYERS must be cade's own, not the layer's: {s}"
    );
}

#[test]
fn restore_after_pure_brings_back_ambient() {
    let sb = Sandbox::new();
    sb.write_snapshot("s2", "AMBIENT=val\u{1f}PWD=/old");
    let out = sb.run(
        &sb.root,
        &["exit", "--shell", "bash"],
        &[
            ("__CADE_SESSION", "s2"),
            ("__CADE_SET", "LAYERVAR"),
            ("__CADE_UNSET", ""),
            ("__CADE_PURE", "1"),
            ("__CADE_HOOKS", "[]"),
            ("__CADE_LAYERS", "x"),
            ("LAYERVAR", "v"),
        ],
    );
    assert!(out.status.success(), "exit failed: {:?}", out);
    let s = stdout(&out);
    // ambient discarded by pure is restored from the snapshot
    assert!(
        s.contains("export AMBIENT='val';"),
        "ambient not restored: {s}"
    );
    // the layer-only var is removed (wasn't in the prior environment)
    assert!(s.contains("unset LAYERVAR;"), "layer var not removed: {s}");
    // PWD was in the snapshot but is shell-managed: not restored
    assert!(!s.contains("PWD"), "restore touched PWD: {s}");
}

#[test]
fn run_caps_at_unapproved_ancestor() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "A=1\n");
    let sub = sb.dir("sub");
    sb.write("sub/.cade", "load env\n");
    sb.write("sub/.env", "B=2\n");

    // allow only the tip: there is no implicit grant to the parent
    sb.allow(&sub);

    // activating at the (unapproved) parent is blocked
    let at_parent = sb.enter(&sb.root, &[]);
    assert!(!at_parent.status.success(), "unapproved parent must block");

    // at the tip it activates, but the run caps below the unapproved parent:
    // only sub's layer composes (B), not the parent's (A)
    let tip_only = sb.enter(&sub, &[]);
    assert!(tip_only.status.success(), "{:?}", tip_only);
    let s = stdout(&tip_only);
    assert!(s.contains("export B='2';"), "child layer missing: {s}");
    assert!(
        !s.contains("export A="),
        "parent layer must not compose yet: {s}"
    );

    // approve the parent too → now both compose
    sb.allow(&sb.root);
    let both = sb.enter(&sub, &[]);
    assert!(stdout(&both).contains("export A='1';"), "{}", stdout(&both));
    assert!(stdout(&both).contains("export B='2';"), "{}", stdout(&both));
}

#[test]
fn allow_gap_fills_up_to_the_approved_base() {
    let sb = Sandbox::new();
    // contiguous chain: root (base) → mid → tip, each with a .cade
    sb.write(".cade", "load env\n");
    sb.write(".env", "BASE=1\n");
    sb.write("mid/.cade", "load env\n");
    sb.write("mid/.env", "MID=1\n");
    let tip = sb.dir("mid/tip");
    sb.write("mid/tip/.cade", "load env\n");
    sb.write("mid/tip/.env", "TIP=1\n");

    // approve the base, then the tip; `mid` is never explicitly allowed
    sb.allow(&sb.root);
    sb.allow(&tip);

    // gap-fill approved `mid`, so the whole stack composes
    let out = sb.enter(&tip, &[]);
    assert!(out.status.success(), "{:?}", out);
    let s = stdout(&out);
    assert!(
        s.contains("export BASE='1';"),
        "base missing (gap-fill failed): {s}"
    );
    assert!(s.contains("export MID='1';"), "gap layer missing: {s}");
    assert!(s.contains("export TIP='1';"), "{s}");
}

#[test]
fn disallowing_a_layer_caps_the_run_below_it() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "A=1\n");
    let sub = sb.dir("sub");
    sb.write("sub/.cade", "load env\n");
    sb.write("sub/.env", "B=2\n");

    sb.allow(&sb.root);
    sb.allow(&sub);
    // disallow the parent layer
    let d = sb.run(&sb.root, &["disallow"], &[]);
    assert!(d.status.success());

    // activating at the disallowed parent is blocked
    let parent = sb.enter(&sb.root, &[]);
    assert!(!parent.status.success(), "disallowed dir must be blocked");

    // at the tip, the run caps below the disallowed parent: sub composes alone
    let tip = sb.enter(&sub, &[]);
    assert!(tip.status.success(), "tip should still activate: {:?}", tip);
    let s = stdout(&tip);
    assert!(s.contains("export B='2';"), "{s}");
    assert!(
        !s.contains("export A="),
        "disallowed parent must be excluded: {s}"
    );
}

#[test]
fn restore_tolerates_missing_prev_snapshot() {
    let sb = Sandbox::new();
    // active per __CADE_LAYERS, session id present, but its snapshot file is
    // gone (corrupted/partial state)
    let out = sb.run(
        &sb.root,
        &["exit", "--shell", "bash"],
        &[
            ("__CADE_SESSION", "ghost-no-file"),
            ("__CADE_SET", "A\u{1f}B"),
            ("__CADE_UNSET", ""),
            ("__CADE_PURE", "0"),
            ("__CADE_HOOKS", "[]"),
            ("__CADE_LAYERS", "x"),
            ("A", "v"),
            ("B", "v"),
        ],
    );
    assert!(
        out.status.success(),
        "restore should not hard-fail: {:?}",
        out
    );
    let s = stdout(&out);
    // with no snapshot, cade-set vars are simply unset and bookkeeping cleaned
    assert!(s.contains("unset A;") && s.contains("unset B;"), "{s}");
    assert!(s.contains("unset __CADE_LAYERS;"), "{s}");
}

#[test]
fn exit_with_no_cade_state_is_a_noop() {
    let sb = Sandbox::new();
    let out = sb.run(&sb.root, &["exit", "--shell", "bash"], &[]);
    assert!(out.status.success(), "no-op exit should succeed: {:?}", out);
    let s = stdout(&out);
    assert!(
        !s.contains("export") && !s.contains("unset"),
        "expected no-op: {s}"
    );
}

#[test]
fn cache_invalidates_when_env_file_changes() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "VAL=one\n");
    sb.allow(&sb.root);

    let first = sb.enter(&sb.root, &[]);
    assert!(stdout(&first).contains("export VAL='one';"));

    // change the value (different length => different size token)
    sb.write(".env", "VAL=changed\n");
    let second = sb.enter(&sb.root, &[]);
    assert!(
        stdout(&second).contains("export VAL='changed';"),
        "cache served a stale value: {}",
        stdout(&second)
    );
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn path_like_var_concats_with_ambient_layer_first() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "PATH=/layer/bin\n");
    sb.allow(&sb.root);

    let out = sb.enter(&sb.root, &[("PATH", "/usr/bin")]);
    assert!(out.status.success(), "{:?}", out);
    // layer prepended, ambient kept (child : … : system)
    assert!(
        stdout(&out).contains("export PATH='/layer/bin:/usr/bin';"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn hard_replace_drops_ambient() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "PATH:=/only/this\n");
    sb.allow(&sb.root);

    let out = sb.enter(&sb.root, &[("PATH", "/usr/bin")]);
    assert!(out.status.success(), "{:?}", out);
    assert!(
        stdout(&out).contains("export PATH='/only/this';"),
        "hard replace should drop ambient: {}",
        stdout(&out)
    );
}

#[test]
fn scalar_var_replaces_ambient() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "EDITOR=vim\n");
    sb.allow(&sb.root);

    let out = sb.enter(&sb.root, &[("EDITOR", "nano")]);
    assert!(out.status.success(), "{:?}", out);
    let s = stdout(&out);
    // not path-like: replace, no nano:vim concatenation
    assert!(s.contains("export EDITOR='vim';"), "{s}");
    assert!(
        !s.contains("EDITOR='nano:vim'") && !s.contains("EDITOR='vim:nano'"),
        "scalar must not absorb ambient: {s}"
    );
}

#[test]
fn concat_uses_snapshot_ambient_so_reloads_dont_grow() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "PATH=/layer/bin\n");
    sb.allow(&sb.root);

    // simulate an already-active reload: the live PATH is already
    // cade-modified, but the session snapshot holds the original ambient.
    sb.write_snapshot("s3", "PATH=/orig");
    let out = sb.enter(
        &sb.root,
        &[
            ("PATH", "/layer/bin:/orig"),
            ("__CADE_SESSION", "s3"),
            ("__CADE_LAYERS", "x"),
        ],
    );
    assert!(out.status.success(), "{:?}", out);
    // ambient taken from the snapshot (/orig), not the live PATH, so no growth
    assert!(
        stdout(&out).contains("export PATH='/layer/bin:/orig';"),
        "concat must use snapshot ambient, not live: {}",
        stdout(&out)
    );
}

#[test]
fn watch_directive_invalidates_a_call_layer() {
    let sb = Sandbox::new();
    // a `call` whose output depends on token.txt, which cade wouldn't otherwise
    // know to watch; `watch` declares that dependency.
    sb.write(
        ".cade",
        "call sh -c \"echo VAL=$(cat token.txt)\"\nwatch token.txt\n",
    );
    sb.write("token.txt", "one");
    sb.allow(&sb.root);

    // the called shell needs a PATH to resolve sh/cat
    let path = std::env::var("PATH").unwrap_or_default();
    let env = [("PATH", path.as_str())];

    let first = sb.enter(&sb.root, &env);
    assert!(first.status.success(), "{:?}", first);
    assert!(
        stdout(&first).contains("export VAL='one';"),
        "{}",
        stdout(&first)
    );

    // change the watched file (different length so the metadata token changes)
    sb.write("token.txt", "twotwo");
    let second = sb.enter(&sb.root, &env);
    assert!(
        stdout(&second).contains("export VAL='twotwo';"),
        "watch did not invalidate the cached call layer: {}",
        stdout(&second)
    );
}

#[test]
fn envrc_is_autodetected_when_no_cade() {
    let sb = Sandbox::new();
    // a bare .envrc, no .cade
    sb.write(".envrc", "dotenv\n");
    sb.write(".env", "FROM_ENVRC=1\n");

    sb.allow(&sb.root);
    let out = sb.enter(&sb.root, &[]);
    assert!(out.status.success(), "envrc activation failed: {:?}", out);
    assert!(
        stdout(&out).contains("export FROM_ENVRC='1';"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn explicit_load_envrc_directive() {
    let sb = Sandbox::new();
    // .cade composes the .envrc as a layer
    sb.write(".cade", "load envrc\n");
    sb.write(".envrc", "export FROM_DIRECTIVE=yes\n");

    sb.allow(&sb.root);
    let out = sb.enter(&sb.root, &[]);
    assert!(out.status.success(), "{:?}", out);
    assert!(stdout(&out).contains("export FROM_DIRECTIVE='yes';"));
}

#[test]
fn envrc_literal_export_and_path_add() {
    let sb = Sandbox::new();
    sb.write(".envrc", "export PLAIN=ok\nPATH_add ./bin\n");
    sb.allow(&sb.root);

    let out = sb.enter(&sb.root, &[]);
    assert!(out.status.success(), "{:?}", out);
    let s = stdout(&out);
    assert!(s.contains("export PLAIN='ok';"), "{s}");
    // PATH_add prepends a dir resolved against the .envrc location
    assert!(s.contains("export PATH=") && s.contains("/bin'"), "{s}");
}

#[test]
fn envrc_unsupported_lines_warn_and_are_skipped() {
    let sb = Sandbox::new();
    // a $-expansion we can't reproduce literally, alongside a line we can
    sb.write(".envrc", "export GOOD=fine\nexport BAD=$HOME/x\n");
    sb.allow(&sb.root);

    let out = sb.enter(&sb.root, &[]);
    assert!(
        out.status.success(),
        "warn-and-continue, not fail: {:?}",
        out
    );
    let s = stdout(&out);
    assert!(s.contains("export GOOD='fine';"), "good line dropped: {s}");
    assert!(
        !s.contains("BAD"),
        "unsupported line should not be applied: {s}"
    );
    // and the user is told, not left guessing
    let e = stderr(&out);
    assert!(
        e.contains("unsupported") && e.contains("$HOME"),
        "expected a warning naming the skipped line: {e}"
    );
}

#[test]
fn inline_assignment_sets_a_var() {
    let sb = Sandbox::new();
    // a bare KEY=value line, no loader needed
    sb.write(".cade", "SOMEVAR=SOMEVAL\n");
    sb.allow(&sb.root);

    let out = sb.enter(&sb.root, &[]);
    assert!(out.status.success(), "enter failed: {:?}", out);
    assert!(
        stdout(&out).contains("export SOMEVAR='SOMEVAL';"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn inline_assignment_hard_replace_drops_ambient() {
    let sb = Sandbox::new();
    sb.write(".cade", "PATH:=/only/this\n");
    sb.allow(&sb.root);

    let out = sb.enter(&sb.root, &[("PATH", "/usr/bin")]);
    assert!(out.status.success(), "{:?}", out);
    assert!(
        stdout(&out).contains("export PATH='/only/this';"),
        "hard replace should drop ambient: {}",
        stdout(&out)
    );
}
