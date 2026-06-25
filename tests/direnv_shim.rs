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

const FAKE_NIX: &str = r#"#!@bash@
set -eu

while [ "$#" -gt 0 ] && [ "$1" != "--command" ]; do
  shift
done
if [ "$#" -eq 0 ]; then
  exit 64
fi
shift

PATH="/fake-dev/bin:${PATH:-}"
export PATH
FROM_FAKE_NIX=ok
export FROM_FAKE_NIX
exec "$@"
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

        Self::with_root_and_cade(root, &fake_cade, bash)
    }

    fn new_with_cade(cade: &Path) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("cade-shim-{}-{id}", std::process::id()));
        fs::create_dir_all(&root).unwrap();

        let bash = bash_path();
        let bash = bash.to_str().expect("bash path must be valid UTF-8");

        Self::with_root_and_cade(root, cade, bash)
    }

    fn with_root_and_cade(root: PathBuf, cade: &Path, bash: &str) -> Self {
        let shim = root.join("direnv");
        let cade = cade.to_str().unwrap();
        write_executable(&shim, &direnv_shim_script(bash, cade, "shim"));

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

fn direnv_shim_script(bash: &str, cade: &str, mode: &str) -> String {
    format!(
        r#"#!{bash}
set -eu

cade={cade:?}
cade_direnv_mode={mode:?}

cmd=${{1:-}}
target=${{2:-}}

case $cmd in
  export)
    case ${{target:-bash}} in
      json)
        CADE_DIRENV=${{CADE_DIRENV:-$cade_direnv_mode}} "$cade" export json
        ;;
      bash | zsh | fish | nushell | nu)
        ;;
      *)
        printf 'direnv shim: unsupported export target: %s\n' "$target" >&2
        exit 1
        ;;
    esac
    ;;
  hook)
    ;;
  *)
    printf 'direnv shim: unsupported command: %s\n' "${{cmd:-<empty>}}" >&2
    exit 1
    ;;
esac
exit 0
"#
    )
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

fn export_json(out: &Output) -> serde_json::Value {
    let mut json: serde_json::Value = serde_json::from_str(stdout(out).trim()).unwrap();
    if let Some(diff) = json.get("DIRENV_DIFF").and_then(|value| value.as_str()) {
        json["DIRENV_DIFF"] = serde_json::from_str(diff).unwrap();
    }
    json
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
fn export_json_loads_cade_shell_through_real_shim() {
    let cade = PathBuf::from(env!("CARGO_BIN_EXE_cade"));
    let sb = ShimSandbox::new_with_cade(&cade);
    let bash = bash_path();
    let bash = bash.to_str().expect("bash path must be valid UTF-8");

    let project = sb.root.join("project");
    let fake_bin = sb.root.join("fake-bin");
    let config_dir = sb.root.join("config").join("cade");
    let state_dir = sb.root.join("state");
    let home = sb.root.join("home");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&fake_bin).unwrap();
    fs::create_dir_all(&config_dir).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::write(project.join(".cade"), "load flake\n").unwrap();
    fs::write(config_dir.join("config.toml"), "direnv = \"shim\"\n").unwrap();
    write_executable(&fake_bin.join("nix"), &FAKE_NIX.replace("@bash@", bash));

    let host_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(fake_bin.clone()).chain(std::env::split_paths(&host_path)),
    )
    .expect("test PATH should be valid");

    let allow = Command::new(&cade)
        .arg("allow")
        .current_dir(&project)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", sb.root.join("config"))
        .env("XDG_STATE_HOME", &state_dir)
        .env("PATH", &path)
        .output()
        .expect("run cade allow");
    assert!(allow.status.success(), "{allow:?}");

    let direct = Command::new(&cade)
        .args(["export", "json"])
        .current_dir(&project)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", sb.root.join("config"))
        .env("XDG_STATE_HOME", &state_dir)
        .env("PATH", &path)
        .output()
        .expect("run direct cade export");
    assert!(direct.status.success(), "{direct:?}");
    assert!(stderr(&direct).is_empty(), "{}", stderr(&direct));

    let shim = Command::new(&sb.shim)
        .args(["export", "json"])
        .current_dir(&project)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", sb.root.join("config"))
        .env("XDG_STATE_HOME", &state_dir)
        .env("PATH", &path)
        .output()
        .expect("run real direnv shim");

    assert!(shim.status.success(), "{shim:?}");
    assert!(stderr(&shim).is_empty(), "{}", stderr(&shim));

    let direct_json = export_json(&direct);
    let shim_json = export_json(&shim);
    assert_eq!(shim_json, direct_json);
    assert_eq!(shim_json["FROM_FAKE_NIX"], "ok");
    assert!(
        shim_json["PATH"]
            .as_str()
            .is_some_and(|path| path.starts_with("/fake-dev/bin:")),
        "{shim_json}"
    );
}

#[test]
fn real_shim_enables_export_without_cade_config() {
    let cade = PathBuf::from(env!("CARGO_BIN_EXE_cade"));
    let sb = ShimSandbox::new_with_cade(&cade);
    let bash = bash_path();
    let bash = bash.to_str().expect("bash path must be valid UTF-8");

    let project = sb.root.join("project-no-config");
    let fake_bin = sb.root.join("fake-bin-no-config");
    let state_dir = sb.root.join("state-no-config");
    let home = sb.root.join("home-no-config");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&fake_bin).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::write(project.join(".cade"), "load flake\n").unwrap();
    write_executable(&fake_bin.join("nix"), &FAKE_NIX.replace("@bash@", bash));

    let host_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(fake_bin.clone()).chain(std::env::split_paths(&host_path)),
    )
    .expect("test PATH should be valid");

    let allow = Command::new(&cade)
        .arg("allow")
        .current_dir(&project)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state_dir)
        .env("PATH", &path)
        .output()
        .expect("run cade allow");
    assert!(allow.status.success(), "{allow:?}");

    let direct = Command::new(&cade)
        .args(["export", "json"])
        .current_dir(&project)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state_dir)
        .env("PATH", &path)
        .output()
        .expect("run direct cade export");
    assert!(direct.status.success(), "{direct:?}");
    assert_eq!(stdout(&direct), "{}\n");

    let shim = Command::new(&sb.shim)
        .args(["export", "json"])
        .current_dir(&project)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state_dir)
        .env("PATH", &path)
        .output()
        .expect("run real direnv shim");
    assert!(shim.status.success(), "{shim:?}");

    let shim_json = export_json(&shim);
    assert_eq!(shim_json["FROM_FAKE_NIX"], "ok");
    assert!(
        shim_json["PATH"]
            .as_str()
            .is_some_and(|path| path.starts_with("/fake-dev/bin:")),
        "{shim_json}"
    );
}
