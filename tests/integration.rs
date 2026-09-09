mod common;

use common::{Sandbox, stderr, stdout};
use std::env::{join_paths, split_paths, var, var_os};
use std::fs::{
    FileTimes, OpenOptions, create_dir_all, metadata, read_dir, read_to_string, set_permissions,
    write,
};
use std::iter::once;
use std::path::{Path, PathBuf};
use std::process::{Output, id};
use std::thread::sleep;
use std::time::Duration;

fn cade_state(sb: &Sandbox) -> PathBuf {
    sb.state.join("cade")
}

fn write_config(sandbox: &Sandbox, contents: &str) -> PathBuf {
    let path = sandbox
        .state
        .join(".config")
        .join("cade")
        .join("config.toml");
    create_dir_all(path.parent().unwrap()).unwrap();
    write(&path, contents).unwrap();
    path
}

fn enter(sandbox: &Sandbox, cwd: &Path, extra_env: &[(&str, &str)]) -> Output {
    sandbox.run(cwd, &["enter", "--shell", "bash"], extra_env)
}

#[test]
fn nested_layers_compose_child_first() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "A=1\nPATH=/parent/bin\n");
    let sub = sb.dir("sub");
    sb.write("sub/.cade", "load env\n");
    sb.write("sub/.env", "B=2\nPATH=/child/bin\n");

    sb.allow(&sb.root);
    sb.allow(&sub);
    let out = enter(&sb, &sub, &[]);
    assert!(out.status.success(), "enter failed: {out:?}");
    let script = stdout(&out);

    assert!(script.contains("export A='1';"), "missing A: {script}");
    assert!(script.contains("export B='2';"), "missing B: {script}");

    assert!(
        script.contains("export PATH='/child/bin:/parent/bin'"),
        "PATH not child-first: {script}"
    );
}

#[test]
fn activation_requires_permission() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "A=1\n");

    let out = enter(&sb, &sb.root, &[]);
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

    sb.allow(&deep);
    let out = enter(&sb, &deep, &[]);
    assert!(out.status.success(), "enter failed: {out:?}");
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
    let out = enter(&sb, &sub, &[("AMBIENT_TEST", "zzz")]);
    assert!(out.status.success(), "enter failed: {out:?}");
    let script = stdout(&out);

    assert!(
        script.contains("unset AMBIENT_TEST;"),
        "ambient not purged: {script}"
    );

    assert!(
        script.contains("export INHERITED='1';"),
        "inherited dropped: {script}"
    );
    assert!(
        script.contains("export CHILD='2';"),
        "child missing: {script}"
    );

    assert!(
        !script.contains("unset PWD;"),
        "must not purge PWD: {script}"
    );
}

#[test]
fn restore_reverts_only_cade_keys_and_leaves_pwd_alone() {
    let sb = Sandbox::new();

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
    assert!(out.status.success(), "exit failed: {out:?}");
    let script = stdout(&out);
    assert!(
        script.contains("export A='old';"),
        "A not restored: {script}"
    );
    assert!(script.contains("unset B;"), "B not unset: {script}");

    assert!(!script.contains("PWD"), "restore touched PWD: {script}");

    assert!(script.contains("unset __CADE_SESSION;"));
}

#[test]
fn first_activation_emits_session_id_not_an_env_blob() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "A=1\n");
    sb.allow(&sb.root);

    let out = enter(&sb, &sb.root, &[("SOMESECRET", "shh")]);
    assert!(out.status.success(), "{out:?}");
    let script = stdout(&out);

    assert!(
        script.contains("export __CADE_SESSION="),
        "no session id: {script}"
    );
    assert!(
        script.contains("export __CADE_STATE_DIR="),
        "no state dir marker: {script}"
    );
    assert!(
        !script.contains("__CADE_PREV"),
        "should not emit the env blob: {script}"
    );

    assert!(
        !script.contains("SOMESECRET"),
        "ambient must not be duplicated into the env: {script}"
    );
}

#[test]
fn nested_shells_share_session_without_corrupting_restore() {
    let sb = Sandbox::new();

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

    let child = sb.run(&sb.root, &["exit", "--shell", "bash"], &active_env);
    assert!(child.status.success(), "{child:?}");
    assert!(
        stdout(&child).contains("export PATH='/orig';"),
        "child restore: {}",
        stdout(&child)
    );

    let parent = sb.run(&sb.root, &["exit", "--shell", "bash"], &active_env);
    assert!(parent.status.success(), "{parent:?}");
    assert!(
        stdout(&parent).contains("export PATH='/orig';"),
        "parent restore must still work after child teardown: {}",
        stdout(&parent)
    );
}

#[test]
fn untrusted_ancestor_layer_is_not_auto_activated() {
    let sb = Sandbox::new();

    sb.write("proj/.cade", "load env\n");
    sb.write("proj/.env", "A=1\n");
    let proj = sb.dir("proj");
    sb.allow(&proj);

    sb.write(".cade", "hook load echo PWNED\n");

    let at_parent = enter(&sb, &sb.root, &[]);
    assert!(
        !at_parent.status.success(),
        "untrusted ancestor must block: {at_parent:?}"
    );

    let at_tip = enter(&sb, &proj, &[]);
    assert!(
        at_tip.status.success(),
        "tip should still activate: {at_tip:?}"
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

    let out = enter(&sb, &sb.root, &[]);
    assert!(out.status.success(), "{out:?}");
    let script = stdout(&out);
    assert!(script.contains("export GOOD='ok';"), "{script}");

    assert!(
        !script.contains("evil"),
        "session/traversal value leaked: {script}"
    );
    assert!(
        !script.contains("export PWD="),
        "PWD must not be layer-set: {script}"
    );
    assert!(
        !script.contains("export SHLVL="),
        "SHLVL must not be layer-set: {script}"
    );
    assert!(
        !script.contains("export __CADE_LAYERS='x';"),
        "__CADE_LAYERS must be cade's own, not the layer's: {script}"
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

    sb.allow(&sub);

    let at_parent = enter(&sb, &sb.root, &[]);
    assert!(!at_parent.status.success(), "unapproved parent must block");

    let tip_only = enter(&sb, &sub, &[]);
    assert!(tip_only.status.success(), "{tip_only:?}");
    let script = stdout(&tip_only);
    assert!(
        script.contains("export B='2';"),
        "child layer missing: {script}"
    );
    assert!(
        !script.contains("export A="),
        "parent layer must not compose yet: {script}"
    );

    sb.allow(&sb.root);
    let both = enter(&sb, &sub, &[]);
    assert!(stdout(&both).contains("export A='1';"), "{}", stdout(&both));
    assert!(stdout(&both).contains("export B='2';"), "{}", stdout(&both));
}

#[test]
fn allow_gap_fills_up_to_the_approved_base() {
    let sb = Sandbox::new();

    sb.write(".cade", "load env\n");
    sb.write(".env", "BASE=1\n");
    sb.write("mid/.cade", "load env\n");
    sb.write("mid/.env", "MID=1\n");
    let tip = sb.dir("mid/tip");
    sb.write("mid/tip/.cade", "load env\n");
    sb.write("mid/tip/.env", "TIP=1\n");

    sb.allow(&sb.root);
    sb.allow(&tip);

    let out = enter(&sb, &tip, &[]);
    assert!(out.status.success(), "{out:?}");
    let script = stdout(&out);
    assert!(
        script.contains("export BASE='1';"),
        "base missing (gap-fill failed): {script}"
    );
    assert!(
        script.contains("export MID='1';"),
        "gap layer missing: {script}"
    );
    assert!(script.contains("export TIP='1';"), "{script}");
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

    let disallow = sb.run(&sb.root, &["disallow"], &[]);
    assert!(disallow.status.success());

    let parent = enter(&sb, &sb.root, &[]);
    assert!(!parent.status.success(), "disallowed dir must be blocked");

    let tip = enter(&sb, &sub, &[]);
    assert!(tip.status.success(), "tip should still activate: {tip:?}");
    let script = stdout(&tip);
    assert!(script.contains("export B='2';"), "{script}");
    assert!(
        !script.contains("export A="),
        "disallowed parent must be excluded: {script}"
    );
}

#[test]
fn restore_tolerates_missing_prev_snapshot() {
    let sb = Sandbox::new();

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
        "restore should not hard-fail: {out:?}"
    );
    let script = stdout(&out);

    assert!(
        script.contains("unset A;") && script.contains("unset B;"),
        "{script}"
    );
    assert!(script.contains("unset __CADE_LAYERS;"), "{script}");
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
    assert!(open.status.success(), "{open:?}");
    let response: serde_json::Value = serde_json::from_str(&stdout(&open)).unwrap();
    let client_id = response["client_id"].as_str().unwrap();
    assert_eq!(response["kind"], "ide");
    assert_eq!(response["project"], project);

    let lease_path = cade_state(&sb)
        .join("leases")
        .join(format!("{client_id}.json"));
    assert!(lease_path.exists(), "lease file missing");
    let before_refresh: serde_json::Value =
        serde_json::from_str(&read_to_string(&lease_path).unwrap()).unwrap();

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
    assert!(refresh.status.success(), "{refresh:?}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stdout(&refresh)).unwrap()["client_id"],
        client_id
    );
    let after_refresh: serde_json::Value =
        serde_json::from_str(&read_to_string(&lease_path).unwrap()).unwrap();
    assert!(
        after_refresh["expires_at"].as_u64().unwrap()
            > before_refresh["expires_at"].as_u64().unwrap(),
        "explicit lease refresh should extend the canonical lease"
    );

    let close = sb.run(&sb.root, &["lease", "close", "--client-id", client_id], &[]);
    assert!(close.status.success(), "{close:?}");
    assert!(!lease_path.exists(), "lease file not removed");
}

#[test]
fn activation_with_client_id_writes_session_lease_holder() {
    let sb = Sandbox::new();
    sb.write(".cade", "A=1\n");
    sb.allow(&sb.root);

    let open = sb.run(&sb.root, &["lease", "open", "--ttl-seconds", "60"], &[]);
    assert!(open.status.success(), "{open:?}");
    let response: serde_json::Value = serde_json::from_str(&stdout(&open)).unwrap();
    let client_id = response["client_id"].as_str().unwrap();
    let lease_path = cade_state(&sb)
        .join("leases")
        .join(format!("{client_id}.json"));
    let before_enter: serde_json::Value =
        serde_json::from_str(&read_to_string(&lease_path).unwrap()).unwrap();

    let out = sb.run(
        &sb.root,
        &["--client-id", client_id, "enter", "--shell", "bash"],
        &[],
    );
    assert!(out.status.success(), "{out:?}");

    let shell_roots = cade_state(&sb).join("gcroots").join("shells");
    let holders: Vec<PathBuf> = read_dir(shell_roots)
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
        serde_json::from_str(&read_to_string(&holders[0]).unwrap()).unwrap();
    assert_eq!(
        session_holder,
        serde_json::json!({ "type": "lease", "client_id": client_id }),
        "session lease holder should only reference the canonical lease"
    );
    let after_enter: serde_json::Value =
        serde_json::from_str(&read_to_string(&lease_path).unwrap()).unwrap();
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
        "stale CADE_CLIENT_ID must not abort activation: {out:?}"
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

    let owner = id().to_string();
    let out = sb.run(
        &sb.root,
        &["--owner-pid", owner.as_str(), "enter", "--shell", "bash"],
        &[],
    );
    assert!(out.status.success(), "{out:?}");

    let shell_roots = cade_state(&sb).join("gcroots").join("shells");
    let process_holders = read_dir(shell_roots)
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

    let owner = id().to_string();
    let out = sb.run(
        &sb.root,
        &["--owner-pid", owner.as_str(), "export", "json"],
        &[("CADE_DIRENV", "full")],
    );
    assert!(out.status.success(), "{out:?}");

    let shell_roots = cade_state(&sb).join("gcroots").join("shells");
    let process_holders = read_dir(shell_roots)
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

    sleep(Duration::from_secs(2));

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
    assert!(out.status.success(), "{out:?}");
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

    let first = enter(&sb, &sb.root, &[]);
    assert!(stdout(&first).contains("export VAL='one';"));

    sb.write(".env", "VAL=changed\n");
    let second = enter(&sb, &sb.root, &[]);
    assert!(
        stdout(&second).contains("export VAL='changed';"),
        "cache served a stale value: {}",
        stdout(&second)
    );
}

#[cfg(unix)]
#[test]
#[expect(
    clippy::literal_string_with_formatting_args,
    reason = "The fixture is a shell script with parameter expansions"
)]
fn timestamp_only_change_reuses_nix_evaluation() {
    use std::os::unix::fs::PermissionsExt as _;

    let sb = Sandbox::new();
    sb.write(".cade", "load flake\n");
    sb.allow(&sb.root);

    let fake_bin = sb.dir("fake-bin");
    let fake_nix = fake_bin.join("nix");
    write(
        &fake_nix,
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = profile ]; then
  exit 0
fi
if [ "${1:-}" != develop ]; then
  printf 'unexpected nix command: %s\n' "$*" >&2
  exit 64
fi
shift
profile=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --command)
      shift
      break
      ;;
    --profile)
      profile="${2:-}"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
if [ -n "$profile" ]; then
  printf 'develop\n' >> "$CADE_FAKE_NIX_CALL_LOG"
  mkdir -p "${profile%/*}"
  env_target="${profile}-env"
  : > "$env_target"
  ln -sfn "$env_target" "$profile"
fi
PATH="/dev/bin:${PATH:-}"
export PATH
FROM_FAKE_NIX=ok
export FROM_FAKE_NIX
exec "$@"
"#,
    )
    .unwrap();
    let mut permissions = metadata(&fake_nix).unwrap().permissions();
    permissions.set_mode(0o755);
    set_permissions(&fake_nix, permissions).unwrap();
    write(fake_bin.join("nix-store"), "#!/bin/sh\nset -eu\nexit 0\n").unwrap();
    let mut store_permissions = metadata(fake_bin.join("nix-store")).unwrap().permissions();
    store_permissions.set_mode(0o755);
    set_permissions(fake_bin.join("nix-store"), store_permissions).unwrap();

    let call_log_path = sb.state.join("nix.log");
    let host_path = var_os("PATH").unwrap_or_default();
    let path = join_paths(once(fake_bin).chain(split_paths(&host_path)))
        .unwrap()
        .to_string_lossy()
        .to_string();
    let call_log_string = call_log_path.to_string_lossy().to_string();
    let env = [
        ("PATH", path.as_str()),
        ("CADE_FAKE_NIX_CALL_LOG", call_log_string.as_str()),
    ];

    let first = enter(&sb, &sb.root, &env);
    let cade_path = sb.root.join(".cade");
    let old_mtime = metadata(&cade_path).unwrap().modified().unwrap();
    let cade_file = OpenOptions::new().write(true).open(&cade_path).unwrap();
    cade_file
        .set_times(FileTimes::new().set_modified(old_mtime + Duration::from_secs(1)))
        .unwrap();
    let second = enter(&sb, &sb.root, &env);

    for out in [&first, &second] {
        assert!(out.status.success(), "{out:?}");
        assert!(
            stdout(out).contains("export FROM_FAKE_NIX='ok';"),
            "{}",
            stdout(out)
        );
    }
    assert_eq!(read_to_string(&call_log_string).unwrap(), "develop\n");
}

#[cfg(unix)]
#[test]
#[expect(
    clippy::literal_string_with_formatting_args,
    reason = "The fixture is a shell script with parameter expansions"
)]
fn nix_shell_hook_runs_on_every_native_and_envrc_entry_without_reevaluating() {
    use std::os::unix::fs::PermissionsExt as _;

    let sb = Sandbox::new();
    sb.write(".cade", "load flake\n");
    sb.allow(&sb.root);

    let fake_bin = sb.dir("fake-bin");
    let fake_nix = fake_bin.join("nix");
    write(
        &fake_nix,
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = profile ]; then
  exit 0
fi
if [ "${1:-}" != develop ]; then
  printf 'unexpected nix command: %s\n' "$*" >&2
  exit 64
fi
shift
profile=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --command)
      shift
      break
      ;;
    --profile)
      profile="${2:-}"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
if [ -n "$profile" ]; then
  printf 'develop\n' >> "$CADE_FAKE_NIX_CALL_LOG"
  mkdir -p "${profile%/*}"
  env_target="${profile}-env"
  : > "$env_target"
  ln -sfn "$env_target" "$profile"
fi
printf "hook-ran\n" >> "$CADE_HOOK_LOG"
PATH="/dev/bin:/path-not-set:${PATH:-}"
export PATH
FROM_SHELL_HOOK=ok
export FROM_SHELL_HOOK
printf "visible shellHook output\n"
exec "$@"
"#,
    )
    .unwrap();
    let mut permissions = metadata(&fake_nix).unwrap().permissions();
    permissions.set_mode(0o755);
    set_permissions(&fake_nix, permissions).unwrap();
    write(fake_bin.join("nix-store"), "#!/bin/sh\nset -eu\nexit 0\n").unwrap();
    let mut store_permissions = metadata(fake_bin.join("nix-store")).unwrap().permissions();
    store_permissions.set_mode(0o755);
    set_permissions(fake_bin.join("nix-store"), store_permissions).unwrap();

    let hook_log_path = sb.state.join("hook.log");
    let call_log_path = sb.state.join("nix.log");
    let host_path = var_os("PATH").unwrap_or_default();
    let path = join_paths(once(fake_bin).chain(split_paths(&host_path)))
        .unwrap()
        .to_string_lossy()
        .to_string();
    let hook_log_string = hook_log_path.to_string_lossy().to_string();
    let call_log_string = call_log_path.to_string_lossy().to_string();
    let env = [
        ("PATH", path.as_str()),
        ("CADE_HOOK_LOG", hook_log_string.as_str()),
        ("CADE_FAKE_NIX_CALL_LOG", call_log_string.as_str()),
    ];

    let first = enter(&sb, &sb.root, &env);
    let second = enter(&sb, &sb.root, &env);

    sb.write(".cade", "load envrc\n");
    sb.write(".envrc", "use flake\n");
    let third = enter(&sb, &sb.root, &env);
    let fourth = enter(&sb, &sb.root, &env);

    for out in [&first, &second, &third, &fourth] {
        assert!(out.status.success(), "{out:?}");
        assert!(
            stdout(out).contains("export FROM_SHELL_HOOK='ok';"),
            "{}",
            stdout(out)
        );
        assert!(
            stderr(out).contains("visible shellHook output"),
            "{}",
            stderr(out)
        );
    }
    assert_eq!(
        read_to_string(&hook_log_string).unwrap(),
        "hook-ran\nhook-ran\nhook-ran\nhook-ran\n"
    );
    assert_eq!(
        read_to_string(&call_log_string).unwrap(),
        "develop\ndevelop\n"
    );
}

fn exported_value(script: &str, key: &str) -> String {
    let prefix = format!("export {key}='");
    assert!(script.contains(&prefix), "missing {key} export in {script}");
    let start = script.find(&prefix).unwrap() + prefix.len();
    let rest = script.get(start..).expect("export value out of bounds");
    assert!(rest.contains("';"), "unterminated {key} export in {script}");
    let end = rest.find("';").unwrap();
    rest.get(..end)
        .expect("export value out of bounds")
        .to_owned()
}

#[test]
fn reload_notices_cade_created_over_implicit_envrc() {
    let sb = Sandbox::new();
    sb.write(".envrc", "export FROM_ENVRC=1\n");
    sb.allow(&sb.root);

    let first = enter(&sb, &sb.root, &[]);
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
    let script = stdout(&reload);
    assert!(
        script.contains("unset FROM_ENVRC;"),
        "reload must restore the envrc variable before reactivation: {script}"
    );
    assert!(
        script.contains("export FROM_CADE='2';"),
        "reload did not pick up the newly-created .cade: {script}"
    );
}

#[test]
fn reload_in_inactive_shell_reminds_for_disallowed_root() {
    let sb = Sandbox::new();
    sb.write(".cade", "A=1\n");

    let out = sb.run(&sb.root, &["reload", "--shell", "bash"], &[]);
    assert!(out.status.success(), "{out:?}");
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
    assert!(out.status.success(), "{out:?}");
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

    sb.write_snapshot("s3", "PATH=/orig");
    let out = enter(
        &sb,
        &sb.root,
        &[
            ("PATH", "/layer/bin:/orig"),
            ("__CADE_SESSION", "s3"),
            ("__CADE_LAYERS", "x"),
        ],
    );
    assert!(out.status.success(), "{out:?}");

    assert!(
        stdout(&out).contains("export PATH='/layer/bin:/orig';"),
        "concat must use snapshot ambient, not live: {}",
        stdout(&out)
    );
}

#[test]
fn reload_into_disallowed_child_keeps_the_approved_parent() {
    let sb = Sandbox::new();
    sb.write(".cade", "A=1\n");
    let sub = sb.dir("sub");
    sb.write("sub/.cade", "B=2\n");
    sb.allow(&sb.root);
    sb.write_snapshot("s5", "PATH=/orig");

    let root_str = sb.root.to_string_lossy().to_string();
    let watches = serde_json::json!({
        "version": "layer-cache-v6",
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
    assert!(out.status.success(), "{out:?}");
    let err = stderr(&out);
    assert!(!err.contains("cade: unloaded"), "{err}");
    assert!(err.contains("disallowed"), "{err}");

    assert!(!stdout(&out).contains("__CADE_LAYERS"), "{}", stdout(&out));
}

#[test]
fn reload_when_parent_revoked_unloads_parent_and_reloads_tip() {
    let sb = Sandbox::new();
    sb.write(".cade", "A=1\n");
    let sub = sb.dir("sub");
    sb.write("sub/.cade", "B=2\n");
    sb.allow(&sub);
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
    assert!(out.status.success(), "{out:?}");
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

    sb.write(
        ".cade",
        "call sh -c \"echo VAL=$(cat token.txt)\"\nwatch token.txt\n",
    );
    sb.write("token.txt", "one");
    sb.allow(&sb.root);

    let path = var("PATH").unwrap_or_default();
    let env = [("PATH", path.as_str())];

    let first = enter(&sb, &sb.root, &env);
    assert!(first.status.success(), "{first:?}");
    assert!(
        stdout(&first).contains("export VAL='one';"),
        "{}",
        stdout(&first)
    );

    sb.write("token.txt", "twotwo");
    let second = enter(&sb, &sb.root, &env);
    assert!(
        stdout(&second).contains("export VAL='twotwo';"),
        "watch did not invalidate the cached call layer: {}",
        stdout(&second)
    );
}

#[test]
fn envrc_is_autodetected_when_no_cade() {
    let sb = Sandbox::new();

    sb.write(".envrc", "dotenv\n");
    sb.write(".env", "FROM_ENVRC=1\n");

    sb.allow(&sb.root);
    let out = enter(&sb, &sb.root, &[]);
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
    write_config(&sb, "direnv = \"none\"\n");

    sb.write(".envrc", "dotenv\n");
    sb.write(".env", "FROM_ENVRC=1\n");

    let out = enter(&sb, &sb.root, &[]);
    let script = stdout(&out);
    assert!(
        !script.contains("FROM_ENVRC"),
        "bare .envrc must not activate when direnv = none: {script}"
    );
    assert!(
        !script.contains("export __CADE_LAYERS="),
        "no layers should compose for a bare .envrc when direnv = none: {script}"
    );
}

#[test]
fn direnv_shim_skips_implicit_envrc_but_export_json_works() {
    let sb = Sandbox::new();
    write_config(&sb, "direnv = \"shim\"\n");
    sb.write(".envrc", "dotenv\n");
    sb.write(".env", "FROM_ENVRC=1\n");

    let entered = enter(&sb, &sb.root, &[]);
    assert!(
        !stdout(&entered).contains("FROM_ENVRC"),
        "shim mode must not implicitly load .envrc: {}",
        stdout(&entered)
    );

    let exported = sb.run(&sb.root, &["export", "json"], &[]);
    assert!(exported.status.success(), "{exported:?}");
    let json: serde_json::Value = serde_json::from_str(stdout(&exported).trim()).unwrap();
    assert!(json.is_object(), "export json must be an object: {json}");
}

#[test]
fn direnv_none_export_json_is_empty_noop() {
    let sb = Sandbox::new();
    write_config(&sb, "direnv = \"none\"\n");
    sb.write(".cade", "A=1\n");
    sb.allow(&sb.root);

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
        .to_owned();

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

    let out = enter(&sb, &sb.root, &[]);
    assert!(!out.status.success(), "missing directed env should fail");
    let err = stderr(&out);

    assert!(
        err.contains("env file") && err.contains("missing.env"),
        "error should name the loader and path: {err}"
    );
}
