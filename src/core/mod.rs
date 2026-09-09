pub mod activation;
pub mod cache;
pub mod enter;
mod layer;
mod participants;
pub mod permissions;
pub mod reload;
pub mod restore;
pub mod sessions;
pub mod shell_state;
mod snapshot;
pub mod status;
mod watch;

use crate::core::cache::{
    ensure_layer_cache_schema, prune_stale_layer_cache, prune_stale_watch_discovery,
};
use crate::{
    progress::{eviction_marker, load_marker},
    shells::ShellOutput,
    types::hook::{HookType, InnerHook},
    verbosity::{self, Verbosity},
};
use anyhow::{Context as _, Result};
use rusqlite::Connection;
use std::env::{current_dir, var, var_os};
use std::fs::create_dir_all;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct Cade {
    db: Connection,
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
    const fn verb(self) -> &'static str {
        match self {
            Self::Loaded => "loaded",
            Self::Reloaded => "reloaded",
        }
    }
}

const fn hook_label(kind: &HookType) -> &'static str {
    match *kind {
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
    let root_text = root.to_string_lossy();
    if var(DISALLOWED_ROOT_MARKER).as_deref() == Ok(root_text.as_ref()) {
        return;
    }

    print!("{}", shell.set_env(DISALLOWED_ROOT_MARKER, &root_text));
    log_disallowed_reminder();
}

fn clear_disallowed_root_marker(shell: &dyn ShellOutput) {
    if var_os(DISALLOWED_ROOT_MARKER).is_some() {
        print!("{}", shell.unset_env(DISALLOWED_ROOT_MARKER));
    }
}

fn log_key_list<Keys, Key>(label: &str, keys: Keys)
where
    Keys: IntoIterator<Item = Key>,
    Key: AsRef<str>,
{
    if !verbosity::enabled(Verbosity::Vars) {
        return;
    }

    let mut sorted: Vec<String> = keys
        .into_iter()
        .map(|key| key.as_ref().to_owned())
        .filter(|key| !key.is_empty())
        .collect();
    sorted.sort_unstable();
    sorted.dedup();
    if !sorted.is_empty() {
        verbosity::log(
            Verbosity::Vars,
            format_args!("cade: {label} {}.", sorted.join(", ")),
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
            eviction_marker(),
            dir,
            layer_count_suffix(total)
        ),
    );
}

fn announce_loaded(dir: &str) {
    verbosity::log(
        Verbosity::Normal,
        format_args!("{}cade: loaded {}.", load_marker(), dir),
    );
}

impl Cade {
    pub fn init() -> Result<Self> {
        let state_dir = if let Some(path) = shell_state::state_dir_from_env() {
            create_dir_all(&path).context("create cade state path")?;
            path
        } else {
            Self::ensure_dir()?
        };
        let db_path = state_dir.join("cade.db");
        let db = Connection::open(db_path)?;
        Self::ensure_db(&db)?;
        Ok(Self {
            db,
            state_dir,
            cwd: current_dir().context("determine cwd")?,
        })
    }

    fn ensure_db(conn: &Connection) -> Result<()> {
        conn.busy_timeout(Duration::from_secs(5))
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
        ensure_layer_cache_schema(conn).context("migrate LayerCache schema")?;
        prune_stale_layer_cache(conn).context("prune stale layer cache entries")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS WatchDiscovery (
                    Dir TEXT PRIMARY KEY,
                    Token TEXT NOT NULL,
                    Files TEXT NOT NULL,
                    LastUsed INTEGER NOT NULL DEFAULT 0
                );",
        )
        .context("create WatchDiscovery table")?;
        prune_stale_watch_discovery(conn).context("prune stale watch discovery entries")?;
        Ok(())
    }

    fn ensure_dir() -> Result<PathBuf> {
        let mut path = if let Ok(xdg) = microxdg::Xdg::new()
            && let Ok(state_dir) = xdg.state()
        {
            state_dir
        } else {
            PathBuf::from("/home")
                .join(whoami::username().context("determine username for cade state path")?)
                .join(".local/state")
        };
        path.push("cade");

        create_dir_all(&path).context("create cade state path")?;
        Ok(path)
    }
}
