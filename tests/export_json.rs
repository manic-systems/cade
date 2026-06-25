mod common;

use common::{Sandbox, stderr, stdout};
use std::{path::Path, process::Output};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn run_export_json(sb: &Sandbox, cwd: &Path, extra_env: &[(&str, &str)]) -> Output {
    let mut env = vec![("CADE_DIRENV", "full")];
    env.extend_from_slice(extra_env);
    sb.run(cwd, &["export", "json"], &env)
}

fn parse_json(out: &Output) -> serde_json::Value {
    serde_json::from_str(stdout(out).trim()).unwrap()
}

fn export_json(sb: &Sandbox, cwd: &Path, extra_env: &[(&str, &str)]) -> serde_json::Value {
    let out = run_export_json(sb, cwd, extra_env);
    assert!(out.status.success(), "{out:?}");
    parse_json(&out)
}

fn direnv_diff(json: &serde_json::Value) -> String {
    json["DIRENV_DIFF"]
        .as_str()
        .expect("json export should carry direnv state")
        .to_string()
}

#[test]
fn json_export_outside_cade_project_is_empty_diff() {
    let sb = Sandbox::new();
    let empty = sb.dir("empty");

    let out = run_export_json(&sb, &empty, &[]);

    assert!(out.status.success(), "{out:?}");
    assert_eq!(stdout(&out), "{}\n");
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));
}

#[test]
fn json_export_uses_session_snapshot_for_concat_baseline() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "PATH=/layer\n");
    sb.write_snapshot("active", "PATH=/usr/bin");
    sb.allow(&sb.root);

    let v = export_json(
        &sb,
        &sb.root,
        &[("__CADE_SESSION", "active"), ("PATH", "/layer:/usr/bin")],
    );
    assert_eq!(v["PATH"], "/layer:/usr/bin");
}

#[test]
fn json_export_missing_session_snapshot_uses_live_baseline() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "PATH=/layer\n");
    sb.allow(&sb.root);

    let v = export_json(
        &sb,
        &sb.root,
        &[("__CADE_SESSION", "missing"), ("PATH", "/usr/bin")],
    );
    assert_eq!(v["PATH"], "/layer:/usr/bin");
}

#[test]
fn json_export_reuses_direnv_baseline_without_growing_path() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "PATH=/layer\n");
    sb.allow(&sb.root);

    let first_json = export_json(&sb, &sb.root, &[("PATH", "/usr/bin")]);
    assert_eq!(first_json["PATH"], "/layer:/usr/bin");
    let direnv_diff = direnv_diff(&first_json);

    let second_json = export_json(
        &sb,
        &sb.root,
        &[("PATH", "/layer:/usr/bin"), ("DIRENV_DIFF", &direnv_diff)],
    );
    assert_eq!(second_json["PATH"], "/layer:/usr/bin");
}

#[test]
fn json_export_state_only_stores_changed_key_preimages() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "PATH=/layer\nA=1\n");
    sb.allow(&sb.root);

    let json = export_json(
        &sb,
        &sb.root,
        &[("PATH", "/usr/bin"), ("SECRET", "do-not-store")],
    );
    let state: serde_json::Value =
        serde_json::from_str(json["DIRENV_DIFF"].as_str().unwrap()).unwrap();

    assert_eq!(state["version"], 2);
    assert_eq!(state["preimage"]["PATH"], "/usr/bin");
    assert!(state["preimage"].get("A").is_some(), "{state}");
    assert!(state["preimage"]["A"].is_null(), "{state}");
    assert!(state["preimage"].get("SECRET").is_none(), "{state}");
    assert!(state.get("baseline").is_none(), "{state}");
}

#[test]
fn json_export_outside_project_restores_previous_direnv_state() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "PATH=/layer\n");
    sb.allow(&sb.root);
    let outside = sb.state.join("outside");
    std::fs::create_dir_all(&outside).unwrap();

    let first_json = export_json(&sb, &sb.root, &[("PATH", "/usr/bin")]);
    let direnv_diff = direnv_diff(&first_json);

    let outside_json = export_json(
        &sb,
        &outside,
        &[("PATH", "/layer:/usr/bin"), ("DIRENV_DIFF", &direnv_diff)],
    );
    assert_eq!(outside_json["PATH"], "/usr/bin");
    assert!(outside_json["DIRENV_DIFF"].is_null(), "{outside_json}");
    assert!(outside_json["DIRENV_DIR"].is_null(), "{outside_json}");
    assert!(outside_json["DIRENV_FILE"].is_null(), "{outside_json}");
    assert!(outside_json["DIRENV_WATCHES"].is_null(), "{outside_json}");
}

#[test]
fn json_export_reenter_after_unload_preserves_baseline_path() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "PATH=/layer\n");
    sb.allow(&sb.root);
    let outside = sb.state.join("outside-reenter");
    std::fs::create_dir_all(&outside).unwrap();

    let first_json = export_json(&sb, &sb.root, &[("PATH", "/usr/bin:/bin")]);
    assert_eq!(first_json["PATH"], "/layer:/usr/bin:/bin");
    let direnv_diff = direnv_diff(&first_json);

    let outside_json = export_json(
        &sb,
        &outside,
        &[
            ("PATH", "/layer:/usr/bin:/bin"),
            ("DIRENV_DIFF", &direnv_diff),
        ],
    );
    assert_eq!(outside_json["PATH"], "/usr/bin:/bin");

    let second_json = export_json(&sb, &sb.root, &[("PATH", "/usr/bin:/bin")]);
    assert_eq!(second_json["PATH"], "/layer:/usr/bin:/bin");
}

#[test]
fn json_export_disallowed_project_restores_previous_direnv_state() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "PATH=/layer\n");
    sb.allow(&sb.root);

    let first_json = export_json(&sb, &sb.root, &[("PATH", "/usr/bin")]);
    let direnv_diff = direnv_diff(&first_json);

    let disallow = sb.run(&sb.root, &["disallow"], &[]);
    assert!(disallow.status.success(), "{disallow:?}");

    let second_json = export_json(
        &sb,
        &sb.root,
        &[("PATH", "/layer:/usr/bin"), ("DIRENV_DIFF", &direnv_diff)],
    );
    assert_eq!(second_json["PATH"], "/usr/bin");
    assert!(second_json["DIRENV_DIFF"].is_null(), "{second_json}");
}

#[test]
fn json_export_disallowed_project_without_previous_state_fails() {
    let sb = Sandbox::new();
    sb.write(".cade", "load env\n");
    sb.write(".env", "PATH=/layer\n");

    let out = run_export_json(&sb, &sb.root, &[("PATH", "/usr/bin")]);

    assert!(!out.status.success(), "{out:?}");
    assert!(
        stderr(&out).contains("run `cade allow`"),
        "{}",
        stderr(&out)
    );
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
}

#[test]
fn json_export_pure_state_restores_ambient_on_unload() {
    let sb = Sandbox::new();
    sb.write(".cade", "pure\nA=1\n");
    sb.allow(&sb.root);
    let outside = sb.state.join("outside-pure");
    std::fs::create_dir_all(&outside).unwrap();

    let first_json = export_json(
        &sb,
        &sb.root,
        &[
            ("AMBIENT_TEST", "old"),
            ("PATH", "/usr/bin"),
            ("HOME", "/home/tester"),
        ],
    );
    assert_eq!(first_json["A"], "1");
    assert!(first_json["AMBIENT_TEST"].is_null(), "{first_json}");
    assert!(first_json["PATH"].is_null(), "{first_json}");
    let first_diff = direnv_diff(&first_json);

    let second_json = export_json(&sb, &sb.root, &[("A", "1"), ("DIRENV_DIFF", &first_diff)]);
    assert_eq!(second_json["A"], "1");
    assert!(second_json["AMBIENT_TEST"].is_null(), "{second_json}");
    assert!(second_json["PATH"].is_null(), "{second_json}");
    let second_diff = direnv_diff(&second_json);

    let outside_json = export_json(&sb, &outside, &[("A", "1"), ("DIRENV_DIFF", &second_diff)]);
    assert!(outside_json["A"].is_null(), "{outside_json}");
    assert_eq!(outside_json["AMBIENT_TEST"], "old");
    assert_eq!(outside_json["PATH"], "/usr/bin");
    assert!(outside_json["DIRENV_DIFF"].is_null(), "{outside_json}");
}

#[cfg(unix)]
#[test]
fn json_export_materializes_nix_shell_with_profile_without_live_holder() {
    let sb = Sandbox::new();
    sb.write(".cade", "load flake\n");
    sb.allow(&sb.root);

    let fake_bin = sb.dir("fake-bin");
    let fake_nix = fake_bin.join("nix");
    let profile_log = sb.state.join("profile.log");
    write_executable(
        &fake_nix,
        r#"#!/bin/sh
set -eu
profile=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --profile)
      shift
      profile="${1:-}"
      ;;
    --command)
      shift
      break
      ;;
  esac
  shift
done
if [ -z "$profile" ]; then
  printf 'missing --profile\n' >&2
  exit 86
fi
printf '%s\n' "$profile" > "$CADE_FAKE_NIX_PROFILE_LOG"
PATH="/materialized/bin:${PATH:-}"
export PATH
FROM_PROFILE_NIX=ok
export FROM_PROFILE_NIX
exec "$@"
"#,
    );

    let host_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(fake_bin.clone()).chain(std::env::split_paths(&host_path)),
    )
    .expect("test PATH should be valid");
    let path = path.to_string_lossy().to_string();
    let profile_log = profile_log.to_string_lossy().to_string();

    let out = sb.run(
        &sb.root,
        &["--owner-pid", "4294967295", "export", "json"],
        &[
            ("CADE_DIRENV", "full"),
            ("PATH", &path),
            ("CADE_FAKE_NIX_PROFILE_LOG", &profile_log),
        ],
    );
    assert!(out.status.success(), "{out:?}");
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));

    let json = parse_json(&out);
    assert_eq!(json["FROM_PROFILE_NIX"], "ok");
    assert!(
        json["PATH"]
            .as_str()
            .is_some_and(|path| path.starts_with("/materialized/bin:")),
        "{json}"
    );

    let logged_profile = std::fs::read_to_string(&profile_log).unwrap();
    assert!(
        logged_profile.contains("/gcroots/shells/direnv-root-")
            && logged_profile.contains("/profiles/"),
        "{logged_profile}"
    );
}

#[cfg(unix)]
fn write_executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}
