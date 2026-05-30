#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

static COUNTER: AtomicU32 = AtomicU32::new(0);

const FAKE_CADE: &str = r#"#!@bash@
set -u

case "${1:-}" in
  export)
    if [ "${2:-}" = "json" ]; then
      case "${CADE_FAKE_MODE:-ok}" in
        missing)
          printf 'no .cade or .envrc found in this directory or any parent\n' >&2
          exit 7
          ;;
        fail)
          printf 'loader failed\n' >&2
          exit 42
          ;;
        session)
          printf '{"SESSION":"%s"}\n' "${__CADE_SESSION-unset}"
          ;;
        *)
          printf '{"A":"1"}\n'
          ;;
      esac
    fi
    ;;
esac
"#;

struct ShimSandbox {
    root: PathBuf,
    shim: PathBuf,
}

impl ShimSandbox {
    fn new() -> Self {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("cade-shim-{}-{id}", std::process::id()));
        fs::create_dir_all(&root).unwrap();

        let bash = bash_path();
        let bash = bash.to_str().expect("bash path must be valid UTF-8");

        let fake_cade = root.join("cade");
        write_executable(&fake_cade, &FAKE_CADE.replace("@bash@", bash));

        let template = include_str!("../nix/direnv-compat.bash");
        let shim = root.join("direnv");
        write_executable(
            &shim,
            &template
                .replace("#!@bash@/bin/bash", &format!("#!{bash}"))
                .replace("@cade@", fake_cade.to_str().unwrap()),
        );

        Self { root, shim }
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with_mode(args, "ok")
    }

    fn run_with_mode(&self, args: &[&str], mode: &str) -> Output {
        let mut command = Command::new(&self.shim);
        command.args(args).env("CADE_FAKE_MODE", mode);
        output_with_retry(command)
    }

    fn run_with_session(&self, args: &[&str], session: &str) -> Output {
        let mut command = Command::new(&self.shim);
        command
            .args(args)
            .env("CADE_FAKE_MODE", "session")
            .env("__CADE_SESSION", session);
        output_with_retry(command)
    }

    fn run_without_path(&self, args: &[&str]) -> Output {
        let mut command = Command::new(&self.shim);
        command.args(args).env_clear().env("CADE_FAKE_MODE", "ok");
        output_with_retry(command)
    }
}

impl Drop for ShimSandbox {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).ok();
    }
}

fn bash_path() -> PathBuf {
    if let Some(path) = std::env::var_os("BASH").map(PathBuf::from)
        && path.is_file()
    {
        return path;
    }

    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("bash"))
        .find(|path| path.is_file())
        .expect("find bash in PATH")
}

fn make_executable(path: &Path) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn write_executable(path: &Path, contents: &str) {
    let mut file = fs::File::create(path).unwrap();
    file.write_all(contents.as_bytes()).unwrap();
    file.sync_all().unwrap();
    drop(file);
    make_executable(path);
}

fn output_with_retry(mut command: Command) -> Output {
    for _ in 0..20 {
        match command.output() {
            Ok(output) => return output,
            Err(err) if err.raw_os_error() == Some(26) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(err) => panic!("run shim: {err}"),
        }
    }
    command.output().expect("run shim")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn unsupported_commands_fail() {
    let sb = ShimSandbox::new();
    let out = sb.run(&["status"]);

    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("unsupported command: status"));
}

#[test]
fn unsupported_export_targets_fail() {
    let sb = ShimSandbox::new();
    let out = sb.run(&["export", "tcsh"]);

    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("unsupported export target: tcsh"));
}

#[test]
fn export_json_delegates_to_cade() {
    let sb = ShimSandbox::new();

    let out = sb.run(&["export", "json"]);
    assert!(out.status.success(), "json export failed: {out:?}");
    assert_eq!(stdout(&out), "{\"A\":\"1\"}\n");

    let out = sb.run_with_mode(&["export", "json"], "fail");
    assert_eq!(out.status.code(), Some(42));
    assert!(stderr(&out).contains("loader failed"));
}

#[test]
fn export_json_preserves_captured_cade_shell_session() {
    let sb = ShimSandbox::new();

    let out = sb.run_with_session(&["export", "json"], "active");

    assert!(out.status.success(), "json export failed: {out:?}");
    assert_eq!(stdout(&out), "{\"SESSION\":\"active\"}\n");
}

#[test]
fn export_json_does_not_need_env_on_path() {
    let sb = ShimSandbox::new();

    let out = sb.run_without_path(&["export", "json"]);

    assert!(out.status.success(), "json export failed: {out:?}");
    assert_eq!(stdout(&out), "{\"A\":\"1\"}\n");
}

#[test]
fn export_json_does_not_interpret_cade_errors() {
    let sb = ShimSandbox::new();

    let out = sb.run_with_mode(&["export", "json"], "missing");

    assert_eq!(out.status.code(), Some(7));
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    assert!(stderr(&out).contains("no .cade or .envrc found"));
}

#[test]
fn shell_export_is_a_harmless_noop_for_captured_shells() {
    let sb = ShimSandbox::new();

    let out = sb.run(&["export", "bash"]);

    assert!(out.status.success(), "{out:?}");
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));
}

#[test]
fn default_export_is_a_harmless_noop_for_captured_shells() {
    let sb = ShimSandbox::new();

    let out = sb.run(&["export"]);

    assert!(out.status.success(), "{out:?}");
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));
}

#[test]
fn shell_hook_is_a_harmless_noop_for_captured_shells() {
    let sb = ShimSandbox::new();

    let out = sb.run(&["hook", "nushell"]);

    assert!(out.status.success(), "{out:?}");
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));
}
