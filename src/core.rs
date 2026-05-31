use crate::{
    config,
    types::{CadeAction, CadeLayer, EnvSet, HookType, InnerHook, Keyword, Loadable},
    verbosity::{self, Verbosity},
};
use anyhow::{Context, Result, anyhow, bail};
use rusqlite::named_params;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

pub struct Cade {
    db: rusqlite::Connection,
    cwd: PathBuf,
    state_dir: PathBuf,
}

// Distinguishes reloads, so we don't double print "unloaded / loaded"
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

pub struct RollupResult {
    pub env: HashMap<String, Vec<String>>,
    // vars that concatenate ambient values rather than clobbering them
    pub absorb: std::collections::HashSet<String>,
    pub unset: Vec<String>,
    pub hooks: Vec<InnerHook>,
    pub purified: bool,
}

/// Vars that get concat applied to them automatically
const PATH_LIKE: &[&str] = &[
    "PATH",
    "MANPATH",
    "INFOPATH",
    "CDPATH",
    "LD_LIBRARY_PATH",
    "DYLD_LIBRARY_PATH",
    "DYLD_FALLBACK_LIBRARY_PATH",
    "LIBRARY_PATH",
    "CPATH",
    "C_INCLUDE_PATH",
    "CPLUS_INCLUDE_PATH",
    "OBJC_INCLUDE_PATH",
    "PKG_CONFIG_PATH",
    "CMAKE_PREFIX_PATH",
    "ACLOCAL_PATH",
    "XDG_DATA_DIRS",
    "XDG_CONFIG_DIRS",
    "TERMINFO_DIRS",
];
const DEFAULT_SHELL_GC_ROOT_TTL_SECS: u64 = 30 * 24 * 3600;

fn shell_gc_root_ttl() -> Duration {
    Duration::from_secs(
        config::shell_gc_root_ttl_seconds().unwrap_or(DEFAULT_SHELL_GC_ROOT_TTL_SECS),
    )
}

#[derive(Debug, Serialize, Deserialize)]
struct WatchEntry {
    path: String,
    mtime: u128,
    size: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct WatchState {
    /// Activation root: the innermost config directory in the hierarchy.
    root: String,
    cade_paths: Vec<String>,
    files: Vec<WatchEntry>,
}

// Keys cade must never set from a layer: the shell owns them, or they're
// cade's own bookkeeping.
fn is_shell_managed(key: &str) -> bool {
    matches!(key, "PWD" | "OLDPWD" | "SHLVL" | "_" | "LAST_EXIT_CODE") || key.starts_with("__CADE_")
}

fn is_pure_preserved_key(key: &str) -> bool {
    is_shell_managed(key)
        || matches!(
            key,
            "HOME"
                | "CADE_VERBOSITY"
                | "CADE_LONG_RUNNING_WARNING_MS"
                | "CADE_SHELL_GC_ROOT_TTL_SECONDS"
        )
}

fn has_config(dir: &Path) -> bool {
    std::fs::exists(dir.join(".cade")).unwrap_or(false)
        || std::fs::exists(dir.join(".envrc")).unwrap_or(false)
}

// Falls back to an implicit `load envrc` when a dir has no .cade.
fn config_keywords(dir: &Path) -> Result<Vec<Keyword>> {
    if std::fs::exists(dir.join(".cade")).unwrap_or(false) {
        read_cade(&dir.join(".cade")).context("reading cade file")
    } else {
        Ok(vec![Keyword::Load(Loadable::Envrc(String::new()))])
    }
}

fn find_cade_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if has_config(&dir) {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

// Reject session ids that could escape the snapshots dir when used as a path.
fn is_valid_session(s: &str) -> bool {
    !s.is_empty() && !s.contains('/') && !s.contains('\\') && !s.contains("..")
}

/// Per-shell-session id, generated at first activation.
fn new_session_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}", std::process::id())
}

/// Read a unit-separated key list
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

fn stable_hash_hex(text: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn layer_uses_nix_loader(keywords: &[Keyword]) -> bool {
    keywords.iter().any(|kw| {
        matches!(
            kw,
            Keyword::Load(
                Loadable::Default | Loadable::Flake(_) | Loadable::Shell(_) | Loadable::Envrc(_)
            )
        )
    })
}

fn session_from_environ(raw: &[u8]) -> Option<String> {
    const PREFIX: &[u8] = b"__CADE_SESSION=";
    raw.split(|&b| b == 0).find_map(|entry| {
        let value = entry.strip_prefix(PREFIX)?;
        let session = std::str::from_utf8(value).ok()?;
        is_valid_session(session).then(|| session.to_string())
    })
}

#[cfg(target_os = "linux")]
fn live_cade_sessions() -> HashSet<String> {
    let mut sessions = HashSet::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return sessions;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(environ) = std::fs::read(entry.path().join("environ")) else {
            continue;
        };
        if let Some(session) = session_from_environ(&environ) {
            sessions.insert(session);
        }
    }

    sessions
}

#[cfg(not(target_os = "linux"))]
fn live_cade_sessions() -> HashSet<String> {
    HashSet::new()
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

    fn snapshot_path(&self, session: &str) -> PathBuf {
        self.state_dir
            .join("snapshots")
            .join(format!("{session}.env"))
    }

    /// Read the pre-activation environment snapshot for a session.
    fn read_snapshot(&self, session: &str) -> HashMap<String, String> {
        if !is_valid_session(session) {
            return HashMap::new();
        }
        std::fs::read_to_string(self.snapshot_path(session))
            .map(|raw| {
                raw.split('\x1F')
                    .filter_map(|e| {
                        e.split_once('=')
                            .map(|(k, v)| (k.to_string(), v.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn write_snapshot(&self, session: &str, env: &HashMap<String, String>) -> Result<()> {
        let dir = self.state_dir.join("snapshots");
        std::fs::create_dir_all(&dir).context("create snapshots dir")?;
        let body = env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("\x1F");
        std::fs::write(self.snapshot_path(session), body).context("write snapshot")
    }

    fn shell_gc_roots_dir(&self) -> PathBuf {
        self.state_dir.join("gcroots").join("shells")
    }

    fn shell_gc_root_session_dir(&self, session: &str) -> PathBuf {
        self.shell_gc_roots_dir().join(session)
    }

    fn gc_state(&self) {
        let live_sessions = live_cade_sessions();
        self.gc_snapshots(&live_sessions);
        self.gc_shell_roots(&live_sessions);
    }

    fn gc_snapshots(&self, live_sessions: &HashSet<String>) {
        let max_age = shell_gc_root_ttl();
        let Ok(entries) = std::fs::read_dir(self.state_dir.join("snapshots")) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let active = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".env"))
                .map(|session| live_sessions.contains(session))
                .unwrap_or(false);
            if active {
                continue;
            }
            let stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .map(|t| t.elapsed().map(|e| e > max_age).unwrap_or(false))
                .unwrap_or(false);
            if stale {
                std::fs::remove_file(path).ok();
            }
        }
    }

    fn gc_shell_roots(&self, live_sessions: &HashSet<String>) {
        let max_age = shell_gc_root_ttl();
        let Ok(entries) = std::fs::read_dir(self.shell_gc_roots_dir()) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(session) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if live_sessions.contains(session) {
                continue;
            }
            let marker = path.join(".last-used");
            let stale = marker
                .metadata()
                .or_else(|_| entry.metadata())
                .and_then(|m| m.modified())
                .map(|t| t.elapsed().map(|e| e > max_age).unwrap_or(false))
                .unwrap_or(false);
            if stale {
                std::fs::remove_dir_all(path).ok();
            }
        }
    }

    fn touch_shell_gc_session(&self, session: &str) -> bool {
        if !is_valid_session(session) {
            return false;
        }
        let session_dir = self.shell_gc_root_session_dir(session);
        if let Err(e) = std::fs::create_dir_all(&session_dir) {
            verbosity::log(
                Verbosity::Normal,
                format_args!(
                    "cade: cannot create nix gc root dir at {}: {e}.",
                    session_dir.display()
                ),
            );
            return false;
        }
        if let Err(e) = std::fs::write(session_dir.join(".last-used"), b"") {
            verbosity::log(
                Verbosity::Normal,
                format_args!(
                    "cade: cannot refresh nix gc root marker at {}: {e}.",
                    session_dir.display()
                ),
            );
            return false;
        }
        true
    }

    fn nix_profile_path(
        &self,
        session: &str,
        layer_count: usize,
        action_index: usize,
        path: &Path,
        spec: &str,
    ) -> Option<PathBuf> {
        if !self.touch_shell_gc_session(session) {
            return None;
        }
        let key = stable_hash_hex(&format!("{}:{spec}", path.display()));
        Some(
            self.shell_gc_root_session_dir(session)
                .join("profiles")
                .join(format!("{layer_count}-{action_index}-{key}")),
        )
    }

    fn root_nix_store_paths(&self, session: &str, paths: &[String]) {
        if paths.is_empty() || !is_valid_session(session) {
            return;
        }

        if !self.touch_shell_gc_session(session) {
            return;
        }
        let session_dir = self.shell_gc_root_session_dir(session);

        let mut unique = paths.to_vec();
        unique.sort_unstable();
        unique.dedup();

        for store_path in unique {
            let path = Path::new(&store_path);
            if !path.exists() {
                verbosity::log(
                    Verbosity::Trace,
                    format_args!("cade: skipping missing nix store path {store_path}."),
                );
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let root = session_dir.join(name);
            if std::fs::symlink_metadata(&root).is_ok() {
                continue;
            }

            let status = match Command::new("nix-store")
                .args(["--add-root"])
                .arg(&root)
                .args(["--indirect", "-r"])
                .arg(&store_path)
                .status()
            {
                Ok(status) => status,
                Err(e) => {
                    verbosity::log(
                        Verbosity::Normal,
                        format_args!("cade: failed to add nix gc root for {store_path}: {e}."),
                    );
                    continue;
                }
            };
            if !status.success() {
                verbosity::log(
                    Verbosity::Normal,
                    format_args!(
                        "cade: nix-store failed to add gc root for {store_path} ({status})."
                    ),
                );
            }
        }
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

    /// Grant or revoke permission for the cwd's activation root.
    ///
    /// On grant, gap-fills from the tip up to the nearest already-approved
    /// ancestor (the base), never approving anything above it.
    pub fn allow_here(&mut self, permission: bool) -> Result<()> {
        let root = find_cade_root(&self.cwd).unwrap_or_else(|| self.cwd.clone());
        if !permission {
            return self.set_permission(&root, false);
        }
        let chain = collect_cade_paths(&root); // tip-first contiguous config dirs
        if chain.is_empty() {
            // if there's no .cade, ignore the request
            return Ok(());
        }
        // the base is the nearest already-approved ancestor; fill from tip to it
        let mut base = None;
        for (i, dir) in chain.iter().enumerate() {
            if self.get_permission(Path::new(dir))? {
                base = Some(i);
                break;
            }
        }
        let upto = base.unwrap_or(1);
        for dir in &chain[0..upto] {
            self.record_permission(Path::new(dir), true)?;
        }
        verbosity::log(
            Verbosity::Normal,
            format_args!(
                "cade is now allowed in {}{}.",
                root.display(),
                if upto > 1 {
                    format!(" (+{} parent layer(s), up to the approved base)", upto - 1)
                } else {
                    String::new()
                }
            ),
        );
        Ok(())
    }

    // Bare db write, without the user-facing message set_permission prints.
    fn record_permission(&self, path: &Path, permission: bool) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO WorkingPaths (Path, Permission) VALUES (:path, :perm);",
            named_params! {
                    ":path": path.to_str().context("parse path as unicode")?,
                    ":perm": permission,
            },
        )?;
        Ok(())
    }

    pub fn set_permission(&mut self, path: &Path, permission: bool) -> Result<()> {
        self.record_permission(path, permission)?;
        verbosity::log(
            Verbosity::Normal,
            format_args!(
                "cade is now {} in {}.",
                if permission { "allowed" } else { "disallowed" },
                path.display()
            ),
        );
        Ok(())
    }

    /// Whether path is explicitly allowed (not by gap-filling)
    pub fn get_permission(&mut self, path: &Path) -> Result<bool> {
        let path_str = path.to_str().context("parse path as unicode")?;
        match self.db.query_one(
            "SELECT Permission FROM WorkingPaths WHERE Path=(:path)",
            &[(":path", &path_str)],
            |row| row.get::<_, bool>(0),
        ) {
            Ok(allowed) => Ok(allowed),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Fetch a cached layer, but only if its token still matches.
    fn get_cached_layer(&self, dir: &str, token: &str) -> Result<Option<CadeLayer>> {
        match self.db.query_row(
            "SELECT Data FROM LayerCache WHERE Dir=(?1) AND Token=(?2)",
            [dir, token],
            |row| row.get::<_, String>(0),
        ) {
            Ok(data) => Ok(serde_json::from_str(&data).ok()),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn store_cached_layer(&self, dir: &str, token: &str, layer: &CadeLayer) -> Result<()> {
        let data = serde_json::to_string(layer)?;
        self.db.execute(
            "INSERT OR REPLACE INTO LayerCache (Dir, Token, Data) VALUES (?1, ?2, ?3)",
            [dir, token, &data],
        )?;
        Ok(())
    }

    pub fn do_activation(
        &mut self,
        shell: &dyn crate::shells::ShellOutput,
        announce: Announce,
    ) -> Result<()> {
        let root = find_cade_root(&self.cwd)
            .context("no .cade or .envrc found in this directory or any parent")?;

        let cade_files = self.approved_chain(&root)?;
        if cade_files.is_empty() {
            bail!(
                "cade is not permitted in {}; use 'cade allow'.",
                root.display()
            );
        }

        let mut cade_layers = Vec::new();
        let mut all_watch_files: Vec<PathBuf> = Vec::new();
        let mut nix_store_paths: Vec<String> = Vec::new();

        // Snapshot the ambient env before activation and allocate any session
        // roots before Nix loaders run, so they can root their full closures.
        self.gc_state();
        let (session, baseline, new_session): (String, HashMap<String, String>, bool) =
            match std::env::var("__CADE_SESSION") {
                Ok(session) if is_valid_session(&session) => {
                    let baseline = self.read_snapshot(&session);
                    (session, baseline, false)
                }
                _ => {
                    let live: HashMap<String, String> = std::env::vars()
                        .filter(|(k, _)| !k.starts_with("__CADE_"))
                        .collect();
                    let session = new_session_id();
                    self.write_snapshot(&session, &live)?;
                    (session, live, true)
                }
            };

        for (layer_count, (path, keywords)) in cade_files.iter().enumerate() {
            let watch_files = watched_files_for_keywords(path, keywords);
            all_watch_files.extend(watch_files.clone());

            let token = compute_layer_key(&watch_files);
            let dir = path.to_string_lossy();
            let cacheable = !layer_uses_nix_loader(keywords);
            if cacheable && let Some(cached) = self.get_cached_layer(&dir, &token)? {
                verbosity::log(
                    Verbosity::Trace,
                    format_args!("cade: using cached layer {}.", path.display()),
                );
                nix_store_paths.extend(cached.nix_store_paths.iter().cloned());
                if cached.nix_store_paths.is_empty() {
                    nix_store_paths
                        .extend(crate::envs::nix_store_paths_from_env_values(&cached.envs));
                }
                cade_layers.push(cached);
            } else {
                verbosity::log(
                    Verbosity::Trace,
                    format_args!("cade: loading layer {}.", path.display()),
                );
                let layer = load_single_layer(layer_count, path, keywords, self, &session)?;
                nix_store_paths.extend(layer.nix_store_paths.iter().cloned());
                if cacheable {
                    self.store_cached_layer(&dir, &token, &layer)?;
                }
                cade_layers.push(layer);
            }
        }

        let rollup = rollup_envs(cade_layers);

        for hook in &rollup.hooks {
            if hook.kind == HookType::LoadPre {
                log_hook(hook);
                print!("{}", shell.emit_hook(&hook.content));
            }
        }

        self.root_nix_store_paths(&session, &nix_store_paths);
        if new_session {
            print!("{}", shell.set_env("__CADE_SESSION", &session));
        }

        output_changes(
            &rollup.env,
            &rollup.absorb,
            &rollup.unset,
            rollup.purified,
            &baseline,
            shell,
        );

        for hook in &rollup.hooks {
            if hook.kind == HookType::LoadPost {
                log_hook(hook);
                print!("{}", shell.emit_hook(&hook.content));
            }
        }

        let layer_paths: Vec<String> = cade_files
            .iter()
            .map(|(p, _)| p.to_string_lossy().to_string())
            .collect();
        print!(
            "{}",
            shell.set_env("__CADE_LAYERS", &layer_paths.join("\x1F"))
        );
        print!(
            "{}",
            shell.set_env("__CADE_STATE_DIR", &self.state_dir.to_string_lossy())
        );
        if let Some(path) = config::current().path.as_deref() {
            print!(
                "{}",
                shell.set_env("__CADE_CONFIG_PATH", &path.to_string_lossy())
            );
        }

        let mut set_keys: Vec<&str> = rollup.env.keys().map(String::as_str).collect();
        set_keys.sort_unstable();
        print!("{}", shell.set_env("__CADE_SET", &set_keys.join("\x1F")));
        print!(
            "{}",
            shell.set_env("__CADE_UNSET", &rollup.unset.join("\x1F"))
        );
        print!(
            "{}",
            shell.set_env("__CADE_PURE", if rollup.purified { "1" } else { "0" })
        );

        // store hooks for restore without file re-reading
        let hooks_json = serde_json::to_string(&rollup.hooks).unwrap_or_default();
        print!("{}", shell.set_env("__CADE_HOOKS", &hooks_json));

        let watch_state = build_watch_state(&root, &all_watch_files);
        let watches_json = serde_json::to_string(&watch_state).unwrap_or_default();
        print!("{}", shell.set_env("__CADE_WATCHES", &watches_json));

        let extra = layer_paths.len().saturating_sub(1);
        verbosity::log(
            Verbosity::Normal,
            format_args!(
                "cade: {} {}{}.",
                announce.verb(),
                root.display(),
                if extra > 0 {
                    format!(" (+{extra} parent layer(s))")
                } else {
                    String::new()
                }
            ),
        );
        log_key_list("set", set_keys);
        log_key_list("cleared", &rollup.unset);

        println!();
        Ok(())
    }

    /// Restore the pre-activation environment. When `finalise` is set this is a
    /// full teardown: it drops __CADE_SESSION and reaps orphaned snapshots (the
    /// active snapshot itself is kept, see below). On a reload, pass
    /// `announce: false` to suppress the unload message.
    pub fn do_restore(
        &mut self,
        shell: &dyn crate::shells::ShellOutput,
        finalise: bool,
        announce: bool,
    ) -> Result<()> {
        let layers = std::env::var("__CADE_LAYERS").ok();
        let session = std::env::var("__CADE_SESSION").ok();

        if layers.is_none() && session.is_none() && std::env::var("__CADE_SET").is_err() {
            return Ok(());
        }

        let prev_env: HashMap<String, String> = session
            .as_deref()
            .map(|s| self.read_snapshot(s))
            .unwrap_or_default();

        let set_keys = read_keylist("__CADE_SET");
        let unset_keys = read_keylist("__CADE_UNSET");
        let pure = std::env::var("__CADE_PURE")
            .map(|v| v == "1")
            .unwrap_or(false);

        let hooks: Vec<InnerHook> = std::env::var("__CADE_HOOKS")
            .ok()
            .and_then(|h| serde_json::from_str(&h).ok())
            .unwrap_or_default();

        if announce
            && verbosity::enabled(Verbosity::Normal)
            && let Some(layers) = &layers
        {
            let paths: Vec<&str> = layers.split('\x1F').filter(|s| !s.is_empty()).collect();
            if let Some(tip) = paths.last() {
                let extra = paths.len().saturating_sub(1);
                verbosity::log(
                    Verbosity::Normal,
                    format_args!(
                        "cade: unloaded {}{}.",
                        tip,
                        if extra > 0 {
                            format!(" (+{extra} parent layer(s))")
                        } else {
                            String::new()
                        }
                    ),
                );
            }
        }

        for hook in &hooks {
            if hook.kind == HookType::UnloadPre {
                log_hook(hook);
                print!("{}", shell.emit_hook(&hook.content));
            }
        }

        if pure {
            // pure discarded the whole ambient env, so restore all of it
            for (k, v) in &prev_env {
                if is_shell_managed(k) {
                    continue;
                }
                print!("{}", shell.set_env(k, v));
            }
            for k in &set_keys {
                if !prev_env.contains_key(k) && !is_shell_managed(k) {
                    print!("{}", shell.unset_env(k));
                }
            }
        } else {
            // revert only the cade env set
            for k in &set_keys {
                if is_shell_managed(k) {
                    continue;
                }
                match prev_env.get(k) {
                    Some(prev_v) => print!("{}", shell.set_env(k, prev_v)),
                    None => print!("{}", shell.unset_env(k)),
                }
            }
        }

        // variables cade `clear`ed are restored from the snapshot
        for k in &unset_keys {
            if is_shell_managed(k) {
                continue;
            }
            if let Some(prev_v) = prev_env.get(k) {
                print!("{}", shell.set_env(k, prev_v));
            }
        }

        for var in [
            "__CADE_LAYERS",
            "__CADE_SET",
            "__CADE_UNSET",
            "__CADE_PURE",
            "__CADE_WATCHES",
            "__CADE_HOOKS",
            "__CADE_STATE_DIR",
            "__CADE_CONFIG_PATH",
        ] {
            print!("{}", shell.unset_env(var));
        }

        // drop session id on finalisation. no snapshot deletion, as
        // nested shells inherit __CADE_SESSION and share this file
        // so deleting it would break the parent's later restore.
        if finalise {
            self.gc_state();
            print!("{}", shell.unset_env("__CADE_SESSION"));
        }

        for hook in &hooks {
            if hook.kind == HookType::UnloadPost {
                log_hook(hook);
                print!("{}", shell.emit_hook(&hook.content));
            }
        }

        log_key_list("restored", &set_keys);
        log_key_list("restored cleared", &unset_keys);

        println!();
        Ok(())
    }

    pub fn do_reload(&mut self, shell: &dyn crate::shells::ShellOutput) -> Result<()> {
        let root = find_cade_root(&self.cwd);
        let is_active = std::env::var("__CADE_LAYERS").is_ok();

        if is_active {
            // reload/restore only when the active state is stale
            let watch_state = std::env::var("__CADE_WATCHES")
                .ok()
                .and_then(|w| serde_json::from_str::<WatchState>(&w).ok());
            let stale = watch_state
                .as_ref()
                .map(|state| watches_are_stale(state, root.as_deref()))
                .unwrap_or(true);

            if stale {
                let reactivating = match &root {
                    Some(r) => self.get_permission(r)?,
                    None => false,
                };
                let same_tree = match (&watch_state, &root) {
                    (Some(state), Some(r)) if reactivating => roots_in_same_cade_tree(state, r),
                    _ => false,
                };
                self.do_restore(shell, !reactivating, !reactivating || !same_tree)?;
                if reactivating {
                    self.do_activation(
                        shell,
                        if same_tree {
                            Announce::Reloaded
                        } else {
                            Announce::Loaded
                        },
                    )?;
                }
            }
        } else {
            self.activate_if_permitted(&root, shell)?;
        }

        Ok(())
    }

    /// Layers to compose, returned parent-first
    fn approved_chain(&mut self, root: &Path) -> Result<Vec<(PathBuf, Vec<Keyword>)>> {
        let mut chain = Vec::new();
        for dir in collect_cade_paths(root) {
            let path = PathBuf::from(&dir);
            if !self.get_permission(&path)? {
                break;
            }
            let keywords = config_keywords(&path)?;
            chain.push((path, keywords));
        }
        chain.reverse(); // parent-first for rollup
        Ok(chain)
    }

    fn activate_if_permitted(
        &mut self,
        root: &Option<PathBuf>,
        shell: &dyn crate::shells::ShellOutput,
    ) -> Result<()> {
        if let Some(root) = root
            && self.get_permission(root)?
        {
            self.do_activation(shell, Announce::Loaded)?;
        }
        Ok(())
    }

    pub fn do_status(&mut self) -> Result<()> {
        let root = find_cade_root(&self.cwd);
        let active = std::env::var("__CADE_LAYERS").is_ok();

        println!("cwd:     {}", self.cwd.display());
        match &root {
            Some(r) => {
                println!("root:    {}", r.display());
                println!("layers (inner \u{2192} outer):");
                let mut capped = false;
                for dir in collect_cade_paths(r) {
                    let allowed = self.get_permission(Path::new(&dir))?;
                    if !allowed {
                        capped = true;
                    }
                    let mark = if !allowed {
                        "not allowed  (run 'cade allow' here)"
                    } else if capped {
                        "allowed, but excluded (a lower layer is not allowed)"
                    } else {
                        "allowed, composed"
                    };
                    println!("  {dir}  [{mark}]");
                }
            }
            None => println!("root:    none (not in a cade project)"),
        }

        println!("active:  {}", if active { "yes" } else { "no" });
        if active {
            let set = read_keylist("__CADE_SET");
            if !set.is_empty() {
                println!("set:     {}", set.join(", "));
            }
            let unset = read_keylist("__CADE_UNSET");
            if !unset.is_empty() {
                println!("cleared: {}", unset.join(", "));
            }
        }
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

fn rollup_envs(cade_layers: Vec<CadeLayer>) -> RollupResult {
    use std::collections::HashSet;
    let mut purified = false;
    let mut env: HashMap<String, Vec<String>> = HashMap::new();
    let mut cleared: HashSet<String> = HashSet::new();
    let mut absorb: HashSet<String> = HashSet::new();
    let mut hooks = Vec::new();
    // Variables treated as lists, some defaults + `concat`ted
    let mut concat_active: HashSet<String> = PATH_LIKE.iter().map(|s| s.to_string()).collect();

    for layer in cade_layers {
        concat_active.extend(layer.concat);

        for var in &layer.clears {
            if is_shell_managed(var) {
                continue;
            }
            env.remove(var);
            absorb.remove(var);
            cleared.insert(var.clone());
        }

        for (k, v) in layer.envs.vars {
            if is_shell_managed(&k) {
                continue;
            }
            cleared.remove(&k);
            // `:=` forces hard replace regardless of other settings
            let is_concat = !layer.envs.hard.contains(&k) && concat_active.contains(&k);
            if is_concat {
                absorb.insert(k.clone());
                let entry = env.entry(k).or_default();
                let mut combined = v;
                combined.append(entry);
                *entry = combined;
            } else {
                // replace drops prior layers and ambient values
                absorb.remove(&k);
                env.insert(k, v);
            }
        }

        if !purified && layer.purify {
            purified = true;
        }
        hooks.extend(layer.hooks);
    }

    // only emit unsets for clears that weren't re-set by a later layer
    let mut unset: Vec<String> = cleared
        .into_iter()
        .filter(|k| !env.contains_key(k))
        .collect();
    unset.sort_unstable();

    RollupResult {
        env,
        absorb,
        unset,
        hooks,
        purified,
    }
}

fn output_changes(
    env: &HashMap<String, Vec<String>>,
    absorb: &std::collections::HashSet<String>,
    unset: &[String],
    purified: bool,
    baseline: &HashMap<String, String>,
    shell: &dyn crate::shells::ShellOutput,
) {
    if purified {
        for (k, _) in std::env::vars() {
            if is_pure_preserved_key(&k) {
                continue;
            }
            print!("{}", shell.unset_env(&k));
        }
    }
    for k in unset {
        print!("{}", shell.unset_env(k));
    }
    for (k, v) in env {
        let mut value = v.join(":");
        // concat vars keep ambient values, appended after .cade values
        if !purified
            && absorb.contains(k)
            && let Some(amb) = baseline.get(k).filter(|a| !a.is_empty())
        {
            value = format!("{value}:{amb}");
        }
        print!("{}", shell.set_env(k, &value));
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

impl CadeLayer {
    pub fn new(_layer: usize, _origin: &Path) -> Self {
        Self {
            envs: EnvSet::new(),
            hooks: Vec::new(),
            purify: false,
            clears: std::collections::HashSet::new(),
            concat: std::collections::HashSet::new(),
            nix_store_paths: Vec::new(),
        }
    }

    pub fn push_action(&mut self, action: CadeAction) {
        use CadeAction::*;
        match action {
            Purify => {
                self.purify = true;
            }
            Environ(env) => {
                self.nix_store_paths.extend(env.nix_store_paths);
                self.envs.hard.extend(env.hard);
                for (k, v) in env.vars {
                    self.envs
                        .vars
                        .entry(k)
                        .and_modify(|iv| iv.extend(v.clone()))
                        .or_insert(v);
                }
            }
            Hook(hook) => {
                self.hooks.push(hook);
            }
            Clear(vars) => {
                self.clears.extend(vars);
            }
            Concat(vars) => {
                self.concat.extend(vars);
            }
        }
    }
}

fn load_single_layer(
    layer_count: usize,
    path: &Path,
    keywords: &[Keyword],
    cade: &Cade,
    session: &str,
) -> Result<CadeLayer> {
    use crate::loaders::*;
    use Keyword::*;
    use Loadable::*;

    let mut layer = CadeLayer::new(layer_count, path);
    for (action_index, kw) in keywords.iter().enumerate() {
        let act = match kw {
            Pure => Ok(CadeAction::Purify),
            Call(argv) => call(path, argv.clone())
                .context("calling process")
                .map(CadeAction::Environ),
            Load(loadable) => match loadable {
                Default => {
                    let profile =
                        cade.nix_profile_path(session, layer_count, action_index, path, "flake");
                    load_flake(path, None, profile).context("loading flake")
                }
                Flake(output) => {
                    let profile = cade.nix_profile_path(
                        session,
                        layer_count,
                        action_index,
                        path,
                        &format!("flake:{output}"),
                    );
                    load_flake(path, Some(output.clone()), profile).context("loading flake")
                }
                Shell(filename) => {
                    let profile = cade.nix_profile_path(
                        session,
                        layer_count,
                        action_index,
                        path,
                        &format!("shell:{filename}"),
                    );
                    load_shell(path, filename.clone(), profile).context("loading shell")
                }
                Env(filename) => load_env(path, filename.clone()).context("loading env file"),
                Envrc(filename) => {
                    let profile_dir = cade.nix_profile_path(
                        session,
                        layer_count,
                        action_index,
                        path,
                        &format!("envrc:{filename}"),
                    );
                    crate::envrc::load_envrc(path, filename.clone(), profile_dir)
                        .context("loading .envrc")
                }
            }
            .map(CadeAction::Environ),
            Hook(hook) => Ok(CadeAction::Hook(hook.clone())),
            Clear(vars) => Ok(CadeAction::Clear(vars.clone())),
            Concat(vars) => Ok(CadeAction::Concat(vars.clone())),
            Set(env) => Ok(CadeAction::Environ(env.clone())),
            Watch(_) => continue,
        }?;
        layer.push_action(act);
    }
    Ok(layer)
}

/// Determine which files a layer depends on
fn watched_files_for_keywords(dir: &Path, keywords: &[Keyword]) -> Vec<PathBuf> {
    let mut files = vec![dir.join(".cade")];
    for kw in keywords {
        match kw {
            Keyword::Load(loadable) => match loadable {
                Loadable::Default | Loadable::Flake(_) => {
                    files.push(dir.join("flake.nix"));
                    files.push(dir.join("flake.lock"));
                }
                Loadable::Shell(f) => {
                    let name = if f.is_empty() {
                        "shell.nix"
                    } else {
                        f.as_str()
                    };
                    files.push(dir.join(name));
                }
                Loadable::Env(f) => {
                    let name = if f.is_empty() { ".env" } else { f.as_str() };
                    files.push(dir.join(name));
                }
                Loadable::Envrc(f) => {
                    files.extend(crate::envrc::envrc_watch_files(dir, f.clone()));
                }
            },
            // explicit user-declared dependencies
            Keyword::Watch(ws) => files.extend(ws.iter().map(|w| dir.join(w))),
            _ => {}
        }
    }
    files
}

fn compute_layer_key(watched_files: &[PathBuf]) -> String {
    let mut parts = Vec::new();
    for file in watched_files {
        if let Ok(meta) = std::fs::metadata(file) {
            parts.push(format!(
                "{}:{}:{}",
                file.display(),
                mtime_nanos(&meta),
                meta.len()
            ));
        }
    }
    parts.join("\n")
}

fn build_watch_state(root: &Path, watched_files: &[PathBuf]) -> WatchState {
    let cade_paths = collect_cade_paths(root);
    let files = watched_files
        .iter()
        .filter_map(|f| {
            let meta = std::fs::metadata(f).ok()?;
            Some(WatchEntry {
                path: f.to_string_lossy().to_string(),
                mtime: mtime_nanos(&meta),
                size: meta.len(),
            })
        })
        .collect();

    WatchState {
        root: root.to_string_lossy().to_string(),
        cade_paths,
        files,
    }
}

/// check if watched files are stale, current_root is the innermost .cade
fn watches_are_stale(state: &WatchState, current_root: Option<&Path>) -> bool {
    let current_root = match current_root {
        Some(r) => r,
        None => return true, // left the cade tree
    };
    if current_root.to_string_lossy() != state.root {
        return true;
    }

    // a .cade added/removed in the ancestry changes the layer set
    if collect_cade_paths(current_root) != state.cade_paths {
        return true;
    }

    // check file mtimes/sizes
    for entry in &state.files {
        match std::fs::metadata(&entry.path) {
            Ok(meta) => {
                if mtime_nanos(&meta) != entry.mtime || meta.len() != entry.size {
                    return true;
                }
            }
            Err(_) => return true, // file disappeared
        }
    }

    false
}

fn roots_in_same_cade_tree(state: &WatchState, current_root: &Path) -> bool {
    let current = current_root.to_string_lossy();
    state.root == current
        || state.cade_paths.iter().any(|p| p == current.as_ref())
        || collect_cade_paths(current_root)
            .iter()
            .any(|p| p == &state.root)
}

/// chain of .cade or .envrcs from root upward (tip-first)
fn collect_cade_paths(root: &Path) -> Vec<String> {
    let mut paths = Vec::new();
    let mut dir = Some(root.to_path_buf());
    while let Some(d) = dir {
        if !has_config(&d) {
            break;
        }
        paths.push(d.to_string_lossy().to_string());
        dir = d.parent().map(Path::to_path_buf);
    }
    paths
}

fn mtime_nanos(meta: &std::fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EnvSet;
    use std::collections::HashMap;

    fn env_layer(pairs: &[(&str, &str)]) -> CadeLayer {
        let mut layer = CadeLayer::new(0, Path::new("/"));
        let mut map = HashMap::new();
        for (k, v) in pairs {
            map.insert(k.to_string(), vec![v.to_string()]);
        }
        layer.push_action(CadeAction::Environ(EnvSet::from_vars(map)));
        layer
    }

    #[test]
    fn path_like_vars_concat_child_first() {
        let parent = env_layer(&[("PATH", "/parent/bin"), ("ONLY_PARENT", "p")]);
        let child = env_layer(&[("PATH", "/child/bin"), ("ONLY_CHILD", "c")]);
        let r = rollup_envs(vec![parent, child]);
        // PATH is path-like: child prepends, so child wins (child : parent)
        assert_eq!(r.env["PATH"], vec!["/child/bin", "/parent/bin"]);
        assert!(r.absorb.contains("PATH"), "PATH should absorb ambient");
        // non-path scalars replace, not concat
        assert_eq!(r.env["ONLY_PARENT"], vec!["p"]);
        assert_eq!(r.env["ONLY_CHILD"], vec!["c"]);
        assert!(!r.absorb.contains("ONLY_PARENT"));
        assert!(!r.purified);
    }

    #[test]
    fn scalar_var_replaces_child_wins() {
        // EDITOR is not path-like: the inner layer replaces, no concatenation
        let parent = env_layer(&[("EDITOR", "nano")]);
        let child = env_layer(&[("EDITOR", "vim")]);
        let r = rollup_envs(vec![parent, child]);
        assert_eq!(r.env["EDITOR"], vec!["vim"]);
        assert!(!r.absorb.contains("EDITOR"));
    }

    #[test]
    fn hard_replace_overrides_concat_default() {
        let parent = env_layer(&[("PATH", "/parent/bin")]);
        let mut child = CadeLayer::new(1, Path::new("/"));
        let mut vars = HashMap::new();
        vars.insert("PATH".to_string(), vec!["/only/child".to_string()]);
        child.push_action(CadeAction::Environ(EnvSet {
            vars,
            hard: std::collections::HashSet::from(["PATH".to_string()]),
            nix_store_paths: Vec::new(),
        }));
        let r = rollup_envs(vec![parent, child]);
        // `:=` hard replace: drops the parent value and won't absorb ambient
        assert_eq!(r.env["PATH"], vec!["/only/child"]);
        assert!(
            !r.absorb.contains("PATH"),
            "hard replace must not absorb ambient"
        );
    }

    #[test]
    fn concat_directive_marks_custom_var() {
        let mut parent = env_layer(&[("MYLIST", "/p")]);
        parent.push_action(CadeAction::Concat(vec!["MYLIST".to_string()]));
        let child = env_layer(&[("MYLIST", "/c")]);
        let r = rollup_envs(vec![parent, child]);
        // marked concat in the parent -> applies inward, child prepends
        assert_eq!(r.env["MYLIST"], vec!["/c", "/p"]);
        assert!(r.absorb.contains("MYLIST"));
    }

    #[test]
    fn clear_removes_inherited_and_is_reported_as_unset() {
        let parent = env_layer(&[("DROP_ME", "x"), ("KEEP", "y")]);
        let mut child = CadeLayer::new(1, Path::new("/"));
        child.push_action(CadeAction::Clear(vec!["DROP_ME".into()]));
        let r = rollup_envs(vec![parent, child]);
        assert!(!r.env.contains_key("DROP_ME"));
        assert!(r.env.contains_key("KEEP"));
        assert_eq!(r.unset, vec!["DROP_ME".to_string()]);
    }

    #[test]
    fn clear_then_reset_in_later_layer_cancels_unset() {
        let l1 = env_layer(&[("X", "1")]);
        let mut l2 = CadeLayer::new(1, Path::new("/"));
        l2.push_action(CadeAction::Clear(vec!["X".into()]));
        let l3 = env_layer(&[("X", "2")]);
        let r = rollup_envs(vec![l1, l2, l3]);
        assert_eq!(r.env["X"], vec!["2"]);
        assert!(r.unset.is_empty(), "X was re-set, so it must not be unset");
    }

    #[test]
    fn pure_flag_does_not_drop_inherited_layers() {
        let parent = env_layer(&[("FROM_PARENT", "kept")]);
        let mut child = CadeLayer::new(1, Path::new("/"));
        child.push_action(CadeAction::Purify);
        child.push_action(CadeAction::Environ(EnvSet::from_vars(HashMap::from([(
            "FROM_CHILD".to_string(),
            vec!["c".to_string()],
        )]))));
        let r = rollup_envs(vec![parent, child]);
        assert!(r.purified);
        // inherited parent-layer var survives pure (pure only discards ambient)
        assert_eq!(r.env["FROM_PARENT"], vec!["kept"]);
        assert_eq!(r.env["FROM_CHILD"], vec!["c"]);
    }

    #[test]
    fn shell_managed_classification() {
        for k in [
            "PWD",
            "OLDPWD",
            "SHLVL",
            "_",
            "LAST_EXIT_CODE",
            "__CADE_PREV",
            "__CADE_SET",
        ] {
            assert!(is_shell_managed(k), "{k} should be shell-managed");
        }
        for k in ["PATH", "HOME", "MY_VAR"] {
            assert!(!is_shell_managed(k), "{k} should not be shell-managed");
        }
        assert!(is_pure_preserved_key("HOME"));
        assert!(is_pure_preserved_key("CADE_SHELL_GC_ROOT_TTL_SECONDS"));
    }

    #[test]
    fn extracts_live_session_from_proc_environ() {
        let raw = b"PATH=/bin\0__CADE_SESSION=123-456\0HOME=/tmp\0";
        assert_eq!(session_from_environ(raw), Some("123-456".to_string()));
    }

    #[test]
    fn ignores_invalid_proc_session_values() {
        assert_eq!(session_from_environ(b"__CADE_SESSION=../bad\0"), None);
        assert_eq!(session_from_environ(b"__CADE_SESSION=\xff\0"), None);
    }

    #[test]
    fn find_cade_root_walks_up_to_innermost() {
        let base = std::env::temp_dir().join(format!("cade-root-{}", std::process::id()));
        let nested = base.join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(base.join("a").join(".cade"), b"").unwrap();

        // from c (no .cade), the innermost ancestor with .cade is a/
        assert_eq!(find_cade_root(&nested), Some(base.join("a")));
        // adding a deeper .cade changes the root
        std::fs::write(base.join("a/b").join(".cade"), b"").unwrap();
        assert_eq!(find_cade_root(&nested), Some(base.join("a/b")));

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn read_cade_errors_on_invalid_utf8_instead_of_truncating() {
        let path = std::env::temp_dir().join(format!("cade-badutf8-{}", std::process::id()));
        // a valid directive, an invalid byte, then another directive that must
        // not be silently dropped
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
