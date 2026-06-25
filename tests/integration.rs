//! End-to-end tests that drive the real `cade` binary against temp directory
//! trees, asserting on the shell statements it emits. Each test gets an
//! isolated state directory (its own permission/cache DB) via XDG_STATE_HOME.

mod common;

use common::{Sandbox, stderr, stdout};
use std::path::{Path, PathBuf};

impl Sandbox {
    fn write_config(&self, contents: &str) -> PathBuf {
        let path = self.state.join(".config").join("cade").join("config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn enter(&self, cwd: &Path, extra_env: &[(&str, &str)]) -> std::process::Output {
        self.run(cwd, &["enter", "--shell", "bash"], extra_env)
    }
}

fn cade_state(sb: &Sandbox) -> PathBuf {
    sb.state.join("cade")
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
    assert!(
        err.contains("cade: disallowed - use \"cade allow\" to load this shell."),
        "unexpected stderr: {err}"
    );
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
        s.contains("export __CADE_STATE_DIR="),
        "no state dir marker: {s}"
    );
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
fn lease_open_refresh_and_close_manage_client_record() {
    let sb = Sandbox::new();
    let project = sb.root.to_string_lossy().to_string();
    let open = sb.run(
        &sb.root,
        &[
            "lease",
            "open",
            "--kind",
            "ide",
            "--project",
            project.as_str(),
            "--ttl-seconds",
            "60",
        ],
        &[],
    );
    assert!(open.status.success(), "{:?}", open);
    let response: serde_json::Value = serde_json::from_str(&stdout(&open)).unwrap();
    let client_id = response["client_id"].as_str().unwrap();
    assert_eq!(response["kind"], "ide");
    assert_eq!(response["project"], project);

    let lease_path = cade_state(&sb)
        .join("leases")
        .join(format!("{client_id}.json"));
    assert!(lease_path.exists(), "lease file missing");
    let before_refresh: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&lease_path).unwrap()).unwrap();

    let refresh = sb.run(
        &sb.root,
        &[
            "lease",
            "refresh",
            "--client-id",
            client_id,
            "--ttl-seconds",
            "120",
        ],
        &[],
    );
    assert!(refresh.status.success(), "{:?}", refresh);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stdout(&refresh)).unwrap()["client_id"],
        client_id
    );
    let after_refresh: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&lease_path).unwrap()).unwrap();
    assert!(
        after_refresh["expires_at"].as_u64().unwrap()
            > before_refresh["expires_at"].as_u64().unwrap(),
        "explicit lease refresh should extend the canonical lease"
    );

    let close = sb.run(&sb.root, &["lease", "close", "--client-id", client_id], &[]);
    assert!(close.status.success(), "{:?}", close);
    assert!(!lease_path.exists(), "lease file not removed");
}

#[test]
fn activation_with_client_id_writes_session_lease_holder() {
    let sb = Sandbox::new();
    sb.write(".cade", "A=1\n");
    sb.allow(&sb.root);

    let open = sb.run(&sb.root, &["lease", "open", "--ttl-seconds", "60"], &[]);
    assert!(open.status.success(), "{:?}", open);
    let response: serde_json::Value = serde_json::from_str(&stdout(&open)).unwrap();
    let client_id = response["client_id"].as_str().unwrap();
    let lease_path = cade_state(&sb)
        .join("leases")
        .join(format!("{client_id}.json"));
    let before_enter: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&lease_path).unwrap()).unwrap();

    let out = sb.run(
        &sb.root,
        &["--client-id", client_id, "enter", "--shell", "bash"],
        &[],
    );
    assert!(out.status.success(), "{:?}", out);

    let shell_roots = cade_state(&sb).join("gcroots").join("shells");
    let holders: Vec<PathBuf> = std::fs::read_dir(shell_roots)
        .unwrap()
        .filter_map(|entry| {
            let path = entry
                .ok()?
                .path()
                .join("holders")
                .join(format!("lease-{client_id}.json"));
            path.exists().then_some(path)
        })
        .collect();
    assert_eq!(holders.len(), 1, "expected one session lease holder");
    let session_holder: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&holders[0]).unwrap()).unwrap();
    assert_eq!(
        session_holder,
        serde_json::json!({ "type": "lease", "client_id": client_id }),
        "session lease holder should only reference the canonical lease"
    );
    let after_enter: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&lease_path).unwrap()).unwrap();
    assert_eq!(
        after_enter, before_enter,
        "enter --client-id should attach to the lease without extending it"
    );
}

#[test]
fn reload_with_stale_client_id_env_still_activates() {
    let sb = Sandbox::new();
    sb.write(".cade", "A=1\n");
    sb.allow(&sb.root);

    let out = sb.run(
        &sb.root,
        &["reload", "--shell", "bash"],
        &[("CADE_CLIENT_ID", "deadbeefdeadbeef")],
    );
    assert!(
        out.status.success(),
        "stale CADE_CLIENT_ID must not abort activation: {:?}",
        out
    );
    assert!(
        stdout(&out).contains("export A='1'"),
        "missing activation despite stale lease: {}",
        stdout(&out)
    );
}

#[test]
fn activation_with_owner_pid_writes_process_holder() {
    let sb = Sandbox::new();
    sb.write(".cade", "A=1\n");
    sb.allow(&sb.root);

    let owner = std::process::id().to_string();
    let out = sb.run(
        &sb.root,
        &["--owner-pid", owner.as_str(), "enter", "--shell", "bash"],
        &[],
    );
    assert!(out.status.success(), "{:?}", out);

    let shell_roots = cade_state(&sb).join("gcroots").join("shells");
    let process_holders = std::fs::read_dir(shell_roots)
        .unwrap()
        .flat_map(|entry| {
            entry
                .unwrap()
                .path()
                .join("holders")
                .read_dir()
                .into_iter()
                .flatten()
        })
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("process-"))
        .count();
    assert_eq!(process_holders, 1, "expected one process holder");
}

#[test]
fn direnv_export_with_owner_pid_writes_session_holder() {
    let sb = Sandbox::new();
    sb.write(".cade", "A=1\n");
    sb.allow(&sb.root);

    let owner = std::process::id().to_string();
    let out = sb.run(
        &sb.root,
        &["--owner-pid", owner.as_str(), "export", "json"],
        &[("CADE_DIRENV", "full")],
    );
    assert!(out.status.success(), "{:?}", out);

    // the direnv export path must scope a session and hold it, so the nix gc
    // roots it creates survive until the holder is gone.
    let shell_roots = cade_state(&sb).join("gcroots").join("shells");
    let process_holders = std::fs::read_dir(shell_roots)
        .unwrap()
        .flat_map(|entry| {
            entry
                .unwrap()
                .path()
                .join("holders")
                .read_dir()
                .into_iter()
                .flatten()
        })
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("process-"))
        .count();
    assert_eq!(
        process_holders, 1,
        "direnv export should write one process holder"
    );
}

#[test]
fn final_restore_keeps_shared_session_snapshot_through_gc() {
    let sb = Sandbox::new();
    let session = "shared";
    sb.write_snapshot(session, "PARENT=original");

    std::thread::sleep(std::time::Duration::from_secs(2));

    let out = sb.run(
        &sb.root,
        &["exit", "--shell", "bash"],
        &[
            ("__CADE_SESSION", session),
            ("__CADE_LAYERS", sb.root.to_str().unwrap()),
            ("__CADE_SET", "PARENT"),
            ("CADE_SHELL_GC_ROOT_TTL_SECONDS", "1"),
        ],
    );
    assert!(out.status.success(), "{:?}", out);
    assert!(
        cade_state(&sb)
            .join("snapshots")
            .join(format!("{session}.env"))
            .exists(),
        "final restore must protect the shared session snapshot while GC runs"
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

fn exported_value(script: &str, key: &str) -> String {
    let prefix = format!("export {key}='");
    let start = script
        .find(&prefix)
        .unwrap_or_else(|| panic!("missing {key} export in {script}"))
        + prefix.len();
    let rest = &script[start..];
    let end = rest
        .find("';")
        .unwrap_or_else(|| panic!("unterminated {key} export in {script}"));
    rest[..end].to_string()
}

#[test]
fn reload_notices_cade_created_over_implicit_envrc() {
    let sb = Sandbox::new();
    sb.write(".envrc", "export FROM_ENVRC=1\n");
    sb.allow(&sb.root);

    let first = sb.enter(&sb.root, &[]);
    assert!(first.status.success(), "{first:?}");
    let first_stdout = stdout(&first);
    assert!(
        first_stdout.contains("export FROM_ENVRC='1';"),
        "{first_stdout}"
    );

    sb.write(".cade", "FROM_CADE=2\n");
    let state_dir = cade_state(&sb).to_string_lossy().to_string();
    let root = sb.root.to_string_lossy().to_string();
    let watches = exported_value(&first_stdout, "__CADE_WATCHES");
    let session = exported_value(&first_stdout, "__CADE_SESSION");
    let hooks = exported_value(&first_stdout, "__CADE_HOOKS");
    let reload = sb.run(
        &sb.root,
        &["reload", "--shell", "bash"],
        &[
            ("__CADE_SESSION", &session),
            ("__CADE_LAYERS", &root),
            ("__CADE_SET", "FROM_ENVRC"),
            ("__CADE_UNSET", ""),
            ("__CADE_PURE", "0"),
            ("__CADE_HOOKS", &hooks),
            ("__CADE_WATCHES", &watches),
            ("__CADE_STATE_DIR", &state_dir),
            ("FROM_ENVRC", "1"),
        ],
    );
    assert!(reload.status.success(), "{reload:?}");
    let s = stdout(&reload);
    assert!(
        s.contains("unset FROM_ENVRC;"),
        "reload must restore the envrc variable before reactivation: {s}"
    );
    assert!(
        s.contains("export FROM_CADE='2';"),
        "reload did not pick up the newly-created .cade: {s}"
    );
}

#[test]
fn reload_in_inactive_shell_reminds_for_disallowed_root() {
    let sb = Sandbox::new();
    sb.write(".cade", "A=1\n");

    let out = sb.run(&sb.root, &["reload", "--shell", "bash"], &[]);
    assert!(out.status.success(), "{:?}", out);
    assert!(
        stderr(&out).contains("cade: disallowed - use \"cade allow\" to load this shell."),
        "{}",
        stderr(&out)
    );
    assert!(
        stdout(&out).contains(&format!(
            "export __CADE_DISALLOWED_ROOT='{}';",
            sb.root.display()
        )),
        "{}",
        stdout(&out)
    );
}

#[test]
fn reload_to_disallowed_root_unloads_and_reminds() {
    let sb = Sandbox::new();
    let allowed = sb.dir("allowed");
    let blocked = sb.dir("blocked");
    sb.write("allowed/.cade", "A=1\n");
    sb.write("blocked/.cade", "B=2\n");
    sb.allow(&allowed);
    sb.write_snapshot("reload-disallowed", "PATH=/orig");

    let allowed_str = allowed.to_string_lossy().to_string();
    let watches = serde_json::json!({
        "root": allowed_str,
        "cade_paths": [allowed_str],
        "files": []
    })
    .to_string();

    let out = sb.run(
        &blocked,
        &["reload", "--shell", "bash"],
        &[
            ("__CADE_SESSION", "reload-disallowed"),
            ("__CADE_SET", "A"),
            ("__CADE_UNSET", ""),
            ("__CADE_PURE", "0"),
            ("__CADE_HOOKS", "[]"),
            ("__CADE_LAYERS", allowed_str.as_str()),
            ("__CADE_WATCHES", watches.as_str()),
            ("A", "1"),
        ],
    );
    assert!(out.status.success(), "{:?}", out);
    let err = stderr(&out);
    assert!(
        err.contains(&format!("cade: unloaded {allowed_str}.")),
        "{err}"
    );
    assert!(
        err.contains("cade: disallowed - use \"cade allow\" to load this shell."),
        "{err}"
    );
    assert!(stdout(&out).contains("unset A;"), "{}", stdout(&out));
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
fn reload_into_disallowed_child_keeps_the_approved_parent() {
    // sitting in a disallowed child while the parent is active must NOT unload
    // the parent; it only prompts to allow the child.
    let sb = Sandbox::new();
    sb.write(".cade", "A=1\n");
    let sub = sb.dir("sub");
    sb.write("sub/.cade", "B=2\n");
    sb.allow(&sb.root); // child intentionally NOT allowed
    sb.write_snapshot("s5", "PATH=/orig");

    let root_str = sb.root.to_string_lossy().to_string();
    let watches = serde_json::json!({
        "version": "layer-cache-v3",
        "root": root_str,
        "cade_paths": [root_str],
        "files": []
    })
    .to_string();

    let out = sb.run(
        &sub,
        &["reload", "--shell", "bash"],
        &[
            ("__CADE_SESSION", "s5"),
            ("__CADE_SET", "A"),
            ("__CADE_UNSET", ""),
            ("__CADE_PURE", "0"),
            ("__CADE_HOOKS", "[]"),
            ("__CADE_LAYERS", root_str.as_str()),
            ("__CADE_WATCHES", watches.as_str()),
            ("A", "1"),
        ],
    );
    assert!(out.status.success(), "{:?}", out);
    let err = stderr(&out);
    assert!(!err.contains("cade: unloaded"), "{err}");
    assert!(err.contains("disallowed"), "{err}");
    // no reactivation: the parent's layer set is left untouched
    assert!(!stdout(&out).contains("__CADE_LAYERS"), "{}", stdout(&out));
}

#[test]
fn reload_when_parent_revoked_unloads_parent_and_reloads_tip() {
    // composed [root, sub]; with the parent no longer approved, the tip stays
    // active but recomposes: unload the dropped parent, reload the tip.
    let sb = Sandbox::new();
    sb.write(".cade", "A=1\n");
    let sub = sb.dir("sub");
    sb.write("sub/.cade", "B=2\n");
    sb.allow(&sub); // parent intentionally NOT allowed (simulating a revoke)
    sb.write_snapshot("s5", "PATH=/orig");

    let root_str = sb.root.to_string_lossy().to_string();
    let sub_str = sub.to_string_lossy().to_string();
    let layers = format!("{root_str}\u{1f}{sub_str}");
    let watches = serde_json::json!({
        "root": sub_str,
        "cade_paths": [sub_str, root_str],
        "files": []
    })
    .to_string();

    let out = sb.run(
        &sub,
        &["reload", "--shell", "bash"],
        &[
            ("__CADE_SESSION", "s5"),
            ("__CADE_SET", "A\u{1f}B"),
            ("__CADE_UNSET", ""),
            ("__CADE_PURE", "0"),
            ("__CADE_HOOKS", "[]"),
            ("__CADE_LAYERS", layers.as_str()),
            ("__CADE_WATCHES", watches.as_str()),
            ("A", "1"),
            ("B", "2"),
        ],
    );
    assert!(out.status.success(), "{:?}", out);
    let err = stderr(&out);
    assert!(err.contains(&format!("cade: unloaded {root_str}")), "{err}");
    assert!(err.contains(&format!("cade: reloaded {sub_str}")), "{err}");
    assert!(
        stdout(&out).contains(&format!("__CADE_LAYERS='{sub_str}'")),
        "{}",
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
    assert!(out.status.success(), "envrc activation failed: {out:?}");
    assert!(
        stdout(&out).contains("export FROM_ENVRC='1';"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn direnv_none_ignores_bare_envrc() {
    let sb = Sandbox::new();
    sb.write_config("direnv = \"none\"\n");
    // a bare .envrc, no .cade
    sb.write(".envrc", "dotenv\n");
    sb.write(".env", "FROM_ENVRC=1\n");

    // with the implicit loader off, the dir is invisible to cade: there is no
    // config root, so nothing activates and no env is emitted.
    let out = sb.enter(&sb.root, &[]);
    let s = stdout(&out);
    assert!(
        !s.contains("FROM_ENVRC"),
        "bare .envrc must not activate when direnv = none: {s}"
    );
    assert!(
        !s.contains("export __CADE_LAYERS="),
        "no layers should compose for a bare .envrc when direnv = none: {s}"
    );
}

#[test]
fn direnv_shim_skips_implicit_envrc_but_export_json_works() {
    let sb = Sandbox::new();
    sb.write_config("direnv = \"shim\"\n");
    sb.write(".envrc", "dotenv\n");
    sb.write(".env", "FROM_ENVRC=1\n");

    // implicit .envrc loader is off in shim mode: the bare .envrc dir is
    // invisible, so entering it does not load anything.
    let entered = sb.enter(&sb.root, &[]);
    assert!(
        !stdout(&entered).contains("FROM_ENVRC"),
        "shim mode must not implicitly load .envrc: {}",
        stdout(&entered)
    );

    // but the export shim endpoint is live
    let exported = sb.run(&sb.root, &["export", "json"], &[]);
    assert!(exported.status.success(), "{exported:?}");
    let json: serde_json::Value = serde_json::from_str(stdout(&exported).trim()).unwrap();
    assert!(json.is_object(), "export json must be an object: {json}");
}

#[test]
fn direnv_none_export_json_is_empty_noop() {
    let sb = Sandbox::new();
    sb.write_config("direnv = \"none\"\n");
    sb.write(".cade", "A=1\n");
    sb.allow(&sb.root);

    // shim off: the endpoint stays a harmless no-op that does not emit cade env
    let out = sb.run(&sb.root, &["export", "json"], &[]);
    assert!(out.status.success(), "{out:?}");
    let json: serde_json::Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(json, serde_json::json!({}), "expected empty delta: {json}");
}

#[test]
fn direnv_none_export_json_unwinds_carried_diff() {
    let sb = Sandbox::new();
    sb.write(".cade", "PROJ_VAR=hello\n");
    sb.allow(&sb.root);

    // First export under an active mode to obtain a real DIRENV_DIFF that
    // carries the project's preimage, exactly as a prior shim/full export would
    // have handed an editor like Zed.
    let active = sb.run(&sb.root, &["export", "json"], &[("CADE_DIRENV", "full")]);
    assert!(active.status.success(), "{active:?}");
    let active_json: serde_json::Value = serde_json::from_str(stdout(&active).trim()).unwrap();
    assert_eq!(
        active_json["PROJ_VAR"], "hello",
        "active export should set the project var: {active_json}"
    );
    let diff = active_json["DIRENV_DIFF"]
        .as_str()
        .expect("active export must carry a DIRENV_DIFF")
        .to_string();

    // Now the mode flips to `none` while the editor still carries that
    // DIRENV_DIFF. The shim is off, but the export must still unwind the carried
    // project env rather than returning `{}` and stranding PROJ_VAR.
    let out = sb.run(
        &sb.root,
        &["export", "json"],
        &[
            ("CADE_DIRENV", "none"),
            ("DIRENV_DIFF", diff.as_str()),
            ("PROJ_VAR", "hello"),
        ],
    );
    assert!(out.status.success(), "{out:?}");
    let json: serde_json::Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_ne!(
        json,
        serde_json::json!({}),
        "off-mode export must unwind a carried diff, not return an empty no-op: {json}"
    );
    let obj = json.as_object().expect("delta is an object");
    assert!(
        obj.contains_key("PROJ_VAR") && json["PROJ_VAR"].is_null(),
        "PROJ_VAR had no preimage, so the unwind must clear it (null): {json}"
    );
    assert!(
        obj.contains_key("DIRENV_DIFF") && json["DIRENV_DIFF"].is_null(),
        "the unwind must clear DIRENV_DIFF: {json}"
    );
}

#[test]
fn directed_load_missing_path_errors_clearly() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env ./conf/missing.env\n");
    sb.allow(&sb.root);

    let out = sb.enter(&sb.root, &[]);
    assert!(!out.status.success(), "missing directed env should fail");
    let err = stderr(&out);
    // a missing directed target is reported by the loader at load time and must
    // still name the offending file clearly
    assert!(
        err.contains("env file") && err.contains("missing.env"),
        "error should name the loader and path: {err}"
    );
}
