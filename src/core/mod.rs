pub mod activation;
pub mod cache;
pub mod enter;
mod layer;
mod participants;
pub mod permissions;
pub mod reload;
pub mod reload_job;
pub mod restore;
pub mod sessions;
pub mod shell_state;
pub mod snapshot;
pub mod status;
mod watch;

use std::{
    env::{
        current_dir,
        var,
        var_os,
    },
    fs::create_dir_all,
    path::{
        Path,
        PathBuf,
    },
    time::Duration,
};

use misstep::{
    OptionExt as _,
    Result,
    ResultExt as _,
};
use rusqlite::{
    Connection,
    config::DbConfig,
};

use crate::{
    config::xdg_dir,
    core::cache::{
        ensure_layer_cache_schema,
        prune_stale_layer_cache,
    },
    progress::{
        eviction_marker,
        load_marker,
    },
    shells::ShellOutput,
    types::hook::{
        HookType,
        InnerHook,
    },
    verbosity::{
        self,
        Verbosity,
    },
};

pub struct Cade {
    db:        Connection,
    cwd:       PathBuf,
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
        let state_dir = Self::state_dir_path()?;
        create_dir_all(&state_dir).context("create cade state path")?;
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
        conn.pragma_update(None, "synchronous", "NORMAL")
            .context("set synchronous")?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .context("disable checkpoint on close")?;

        let auto_vacuum = conn
            .pragma_query_value(None, "auto_vacuum", |row| row.get::<_, i64>(0))
            .context("read auto_vacuum")?;

        if auto_vacuum != 2 {
            // sqlite only changes an existing database's auto_vacuum mode on a
            // VACUUM.
            conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")
                .context("set auto_vacuum")?;
            conn.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")
                .context("vacuum into incremental auto_vacuum")?;
        }

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

        conn.execute_batch("PRAGMA incremental_vacuum")
            .context("release free pages")?;
        Ok(())
    }

    pub fn state_dir_path() -> Result<PathBuf> {
        if let Some(path) = shell_state::state_dir_from_env() {
            return Ok(path);
        }

        Ok(xdg_dir("XDG_STATE_HOME", ".local/state")
            .context("find cade state path, neither XDG_STATE_HOME nor HOME is set")?
            .join("cade"))
    }
}
