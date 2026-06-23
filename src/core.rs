mod activation;
mod cache;
mod enter;
mod layer;
mod participants;
mod permissions;
mod reload;
mod restore;
mod sessions;
mod shell_state;
mod snapshot;
mod status;
mod watch;

use participants::{find_cade_root, participant_dirs};
use sessions::is_valid_session;
use watch::WatchState;

use crate::{
    shells::ShellOutput,
    types::{HookType, InnerHook, Keyword},
    verbosity::{self, Verbosity},
};
use anyhow::{Context, Result};
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

impl Cade {
    pub fn init() -> anyhow::Result<Cade> {
        let state_dir = if let Some(path) = shell_state::state_dir_from_env() {
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
                    Data TEXT NOT NULL,
                    LastUsed INTEGER NOT NULL DEFAULT 0
                );",
        )
        .context("create LayerCache table")?;
        Cade::ensure_layer_cache_schema(conn).context("migrate LayerCache schema")?;
        Cade::prune_stale_layer_cache(conn).context("prune stale layer cache entries")?;
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
