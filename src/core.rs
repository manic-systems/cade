mod activation;
mod cache;
mod layer;
mod lifecycle;
mod participants;
mod permissions;
mod rollup;
mod sessions;
mod snapshot;
mod watch;

use participants::{find_cade_root, participant_dirs};
use sessions::is_valid_session;
use watch::{WatchState, build_watch_state, files_changed};

use crate::{
    shells::ShellOutput,
    types::{HookType, InnerHook, Keyword, Loadable},
    verbosity::{self, Verbosity},
};
use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};

pub struct Cade {
    db: rusqlite::Connection,
    cwd: PathBuf,
    state_dir: PathBuf,
}

const DISALLOWED_REMINDER: &str = "cade: disallowed - use \"cade allow\" to load this shell.";
const DISALLOWED_ROOT_MARKER: &str = "__CADE_DISALLOWED_ROOT";

#[derive(Clone, Copy)]
pub enum Announce {
    Loaded,
    Reloaded,
}

impl Announce {
    fn verb(self) -> &'static str {
        match self {
            Announce::Loaded => "loaded",
            Announce::Reloaded => "reloaded",
        }
    }
}

fn hook_label(kind: &HookType) -> &'static str {
    match kind {
        HookType::LoadPre => "preload",
        HookType::LoadPost => "load",
        HookType::UnloadPre => "preunload",
        HookType::UnloadPost => "unload",
    }
}

fn log_hook(hook: &InnerHook) {
    verbosity::log(
        Verbosity::Trace,
        format_args!(
            "cade: running {} hook: {}",
            hook_label(&hook.kind),
            hook.content
        ),
    );
}

fn log_disallowed_reminder() {
    verbosity::log(Verbosity::Normal, format_args!("{DISALLOWED_REMINDER}"));
}

fn mark_disallowed_root(root: &Path, shell: &dyn ShellOutput) {
    let root = root.to_string_lossy();
    if std::env::var(DISALLOWED_ROOT_MARKER).as_deref() == Ok(root.as_ref()) {
        return;
    }

    print!("{}", shell.set_env(DISALLOWED_ROOT_MARKER, &root));
    log_disallowed_reminder();
}

fn clear_disallowed_root_marker(shell: &dyn ShellOutput) {
    if std::env::var_os(DISALLOWED_ROOT_MARKER).is_some() {
        print!("{}", shell.unset_env(DISALLOWED_ROOT_MARKER));
    }
}

fn log_key_list<I, S>(label: &str, keys: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    if !verbosity::enabled(Verbosity::Vars) {
        return;
    }

    let mut keys: Vec<String> = keys
        .into_iter()
        .map(|k| k.as_ref().to_owned())
        .filter(|k| !k.is_empty())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    if !keys.is_empty() {
        verbosity::log(
            Verbosity::Vars,
            format_args!("cade: {label} {}.", keys.join(", ")),
        );
    }
}

fn layer_count_suffix(total: usize) -> String {
    if total > 1 {
        format!(" ({total})")
    } else {
        String::new()
    }
}

fn announce_unloaded(dir: &str, total: usize) {
    verbosity::log(
        Verbosity::Normal,
        format_args!(
            "{}cade: unloaded {}{}.",
            crate::progress::eviction_marker(),
            dir,
            layer_count_suffix(total)
        ),
    );
}

fn announce_loaded(dir: &str) {
    verbosity::log(
        Verbosity::Normal,
        format_args!("{}cade: loaded {}.", crate::progress::load_marker(), dir),
    );
}

// implicit envrc when .cade is absent
fn config_keywords(dir: &Path) -> Result<Vec<Keyword>> {
    let mut keywords = if std::fs::exists(dir.join(".cade")).unwrap_or(false) {
        read_cade(&dir.join(".cade")).context("reading cade file")?
    } else {
        vec![Keyword::Load(Loadable::Envrc(String::new()))]
    };
    for kw in &mut keywords {
        crate::expand::expand_keyword(kw);
    }
    Ok(keywords)
}

fn read_keylist(var: &str) -> Vec<String> {
    std::env::var(var)
        .ok()
        .map(|raw| {
            raw.split('\x1F')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

impl Cade {
    pub fn init() -> anyhow::Result<Cade> {
        let state_dir = if let Ok(dir) = std::env::var("__CADE_STATE_DIR") {
            let path = PathBuf::from(dir);
            std::fs::create_dir_all(&path).context("create cade state path")?;
            path
        } else {
            Cade::ensure_dir()?
        };
        let db_path = state_dir.join("cade.db");
        let mut db = rusqlite::Connection::open(db_path)?;
        Cade::ensure_db(&mut db)?;
        Ok(Self {
            db,
            state_dir,
            cwd: std::env::current_dir().context("determine cwd")?,
        })
    }

    fn ensure_db(conn: &mut rusqlite::Connection) -> Result<()> {
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .context("set busy_timeout")?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("enable WAL")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS WorkingPaths (
                    Path TEXT PRIMARY KEY,
                    Permission INTEGER NOT NULL DEFAULT 0
                );",
        )
        .context("create WorkingPaths table")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS LayerCache (
                    Dir TEXT PRIMARY KEY,
                    Token TEXT NOT NULL,
                    Data TEXT NOT NULL
                );",
        )
        .context("create LayerCache table")?;
        Ok(())
    }

    fn ensure_dir() -> Result<PathBuf> {
        let mut path = if let Ok(xdg) = microxdg::Xdg::new()
            && let Ok(state_dir) = xdg.state()
        {
            state_dir
        } else {
            let mut p = PathBuf::from("/home");
            p.push(whoami::username());
            p.push(".local");
            p.push("state");
            p
        };
        path.push("cade");

        std::fs::create_dir_all(&path).context("create cade state path")?;
        Ok(path)
    }
}

pub fn read_cade(path: &Path) -> Result<Vec<Keyword>> {
    let contents = std::fs::read(path).context("reading cade file")?;
    let mut accum = Vec::new();
    for (n, raw) in contents.split(|&b| b == b'\n').enumerate() {
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let line = std::str::from_utf8(raw).map_err(|e| {
            anyhow!(
                "parse cade file at {}: line {} is not valid UTF-8: {e}",
                path.display(),
                n + 1
            )
        })?;
        match line.parse::<Keyword>() {
            Ok(kw) => accum.push(kw),
            Err(crate::cli::parse::ParseError::EmptyLine) => continue,
            Err(e) => {
                return Err(anyhow!(
                    "parse cade file at {}: line {}: {e}",
                    path.display(),
                    n + 1
                ));
            }
        }
    }
    Ok(accum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_cade_errors_on_invalid_utf8_instead_of_truncating() {
        let path = std::env::temp_dir().join(format!("cade-badutf8-{}", std::process::id()));
        let mut body = b"FOO=bar\n".to_vec();
        body.extend_from_slice(&[0xff, b'\n']);
        body.extend_from_slice(b"pure\n");
        std::fs::write(&path, &body).unwrap();

        let err = read_cade(&path).expect_err("invalid UTF-8 must be an error");
        assert!(
            err.to_string().contains("line 2"),
            "error should point at the bad line: {err}"
        );

        std::fs::remove_file(&path).ok();
    }
}
