mod activation;
mod participants;

use participants::{find_cade_root, participant_dirs};

use crate::{
    config,
    env_delta::is_shell_managed,
    shells::ShellOutput,
    types::{CadeAction, CadeLayer, EnvSet, HookType, InnerHook, Keyword, Loadable},
    verbosity::{self, Verbosity},
};
use anyhow::{Context, Result, anyhow, bail};
use rusqlite::named_params;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub struct Cade {
    db: rusqlite::Connection,
    cwd: PathBuf,
    state_dir: PathBuf,
}

const DISALLOWED_REMINDER: &str = "cade: disallowed - use \"cade allow\" to load this shell.";
const DISALLOWED_ROOT_MARKER: &str = "__CADE_DISALLOWED_ROOT";

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

/// ` (n)` stack-depth badge; empty for a lone layer
fn layer_count_suffix(total: usize) -> String {
    if total > 1 {
        format!(" ({total})")
    } else {
        String::new()
    }
}

/// yellow `[←]` unloaded notice; `total` sizes the badge
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

/// green `[→]` loaded notice for an in-place single-layer change
fn announce_loaded(dir: &str) {
    verbosity::log(
        Verbosity::Normal,
        format_args!("{}cade: loaded {}.", crate::progress::load_marker(), dir),
    );
}

pub struct RollupResult {
    pub env: HashMap<String, Vec<String>>,
    // vars that concatenate ambient values rather than clobbering them
    pub absorb: HashSet<String>,
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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Write via a same-dir temp file and rename so a concurrent reader (the GC
/// scan) never observes a half-written state file.
fn atomic_write(path: &Path, body: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path.file_name().and_then(|n| n.to_str()).unwrap_or("cade");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = parent.join(format!(".{stem}.tmp.{}.{nanos}", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, body).and_then(|()| std::fs::rename(&tmp, path)) {
        std::fs::remove_file(&tmp).ok();
        return Err(e);
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SessionHolder {
    Process {
        pid: u32,
        start_time: String,
        last_seen: u64,
    },
    Lease {
        client_id: String,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct LeaseRecord {
    client_id: String,
    kind: String,
    project: Option<String>,
    expires_at: u64,
    last_seen: u64,
}

impl LeaseRecord {
    fn session_holder(&self) -> SessionHolder {
        SessionHolder::Lease {
            client_id: self.client_id.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
struct LeaseResponse {
    client_id: String,
    kind: String,
    project: Option<String>,
    expires_at: u64,
}

impl From<&LeaseRecord> for LeaseResponse {
    fn from(lease: &LeaseRecord) -> Self {
        Self {
            client_id: lease.client_id.clone(),
            kind: lease.kind.clone(),
            project: lease.project.clone(),
            expires_at: lease.expires_at,
        }
    }
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

// Falls back to an implicit `load envrc` when a dir has no .cade.
fn config_keywords(dir: &Path) -> Result<Vec<Keyword>> {
    if std::fs::exists(dir.join(".cade")).unwrap_or(false) {
        read_cade(&dir.join(".cade")).context("reading cade file")
    } else {
        Ok(vec![Keyword::Load(Loadable::Envrc(String::new()))])
    }
}

// Reject session ids that could escape the snapshots dir when used as a path.
fn is_valid_session(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn is_valid_client_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn validate_client_id(s: &str) -> Result<()> {
    if is_valid_client_id(s) {
        Ok(())
    } else {
        bail!("invalid cade lease client id")
    }
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf))
        .is_err()
    {
        let seed = format!("{}-{}-{}", std::process::id(), now_secs(), new_session_id());
        for (i, byte) in seed.as_bytes().iter().enumerate() {
            buf[i % bytes] ^= *byte;
            buf[(i * 7 + 3) % bytes] = buf[(i * 7 + 3) % bytes].wrapping_add(*byte);
        }
    }
    buf.into_iter().map(|b| format!("{b:02x}")).collect()
}

fn new_client_id() -> String {
    random_hex(16)
}

/// Per-shell-session id, generated at first activation.
fn new_session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}", std::process::id())
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

#[cfg(target_os = "linux")]
fn parse_proc_stat(raw: &str) -> Option<(u32, String)> {
    let end = raw.rfind(") ")?;
    let fields: Vec<&str> = raw[end + 2..].split_whitespace().collect();
    let ppid = fields.get(1)?.parse::<u32>().ok()?;
    let start_time = fields.get(19)?.to_string();
    Some((ppid, start_time))
}

#[cfg(target_os = "linux")]
fn process_start_time(pid: u32) -> Option<String> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_proc_stat(&raw).map(|(_, start)| start)
}

#[cfg(target_os = "macos")]
fn process_start_time(pid: u32) -> Option<String> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let info_len = std::mem::size_of::<libc::proc_bsdinfo>();
    let rc = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast::<libc::c_void>(),
            info_len as libc::c_int,
        )
    };
    if rc < info_len as libc::c_int {
        return None;
    }
    let info = unsafe { info.assume_init() };
    (info.pbi_pid == pid as u32)
        .then(|| format!("{}-{}", info.pbi_start_tvsec, info.pbi_start_tvusec))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_start_time(_pid: u32) -> Option<String> {
    None
}

#[cfg(unix)]
fn parent_pid() -> Option<u32> {
    u32::try_from(unsafe { libc::getppid() }).ok()
}

#[cfg(not(unix))]
fn parent_pid() -> Option<u32> {
    None
}

fn process_holder_is_live(pid: u32, start_time: &str) -> bool {
    process_start_time(pid)
        .map(|current| current == start_time)
        .unwrap_or(false)
}

fn lease_record_is_live(lease: &LeaseRecord) -> bool {
    lease.expires_at > now_secs()
}

fn holder_file_name(holder: &SessionHolder) -> Result<String> {
    match holder {
        SessionHolder::Process {
            pid, start_time, ..
        } => {
            if start_time
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            {
                Ok(format!("process-{pid}-{start_time}.json"))
            } else {
                bail!("invalid process start time")
            }
        }
        SessionHolder::Lease { client_id } => {
            validate_client_id(client_id)?;
            Ok(format!("lease-{client_id}.json"))
        }
    }
}

fn configured_client_id(explicit: Option<&str>) -> Option<String> {
    explicit
        .map(str::to_owned)
        .or_else(|| std::env::var("CADE_CLIENT_ID").ok())
        .filter(|id| !id.is_empty())
}

/// A stable session id for the direnv export path, which cannot persist
/// `__CADE_SESSION` between calls. Scoped to the holding lease if there is one,
/// otherwise the owning shell process; `None` when neither is resolvable, in
/// which case the export skips gc rooting.
fn direnv_session_id(client_id: Option<&str>, owner_pid: Option<u32>) -> Option<String> {
    if let Some(client_id) = configured_client_id(client_id) {
        return Some(format!("direnv-lease-{}", stable_hash_hex(&client_id)));
    }
    let pid = owner_pid.or_else(parent_pid)?;
    let start_time = process_start_time(pid)?;
    Some(format!("direnv-{pid}-{}", stable_hash_hex(&start_time)))
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
    fn read_snapshot(&self, session: &str) -> Option<HashMap<String, String>> {
        if !is_valid_session(session) {
            return None;
        }
        std::fs::read_to_string(self.snapshot_path(session))
            .ok()
            .map(|raw| {
                raw.split('\x1F')
                    .filter_map(|e| {
                        e.split_once('=')
                            .map(|(k, v)| (k.to_string(), v.to_string()))
                    })
                    .collect()
            })
    }

    fn write_snapshot(&self, session: &str, env: &HashMap<String, String>) -> Result<()> {
        let dir = self.state_dir.join("snapshots");
        std::fs::create_dir_all(&dir).context("create snapshots dir")?;
        let body = env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("\x1F");
        atomic_write(&self.snapshot_path(session), body.as_bytes()).context("write snapshot")
    }

    fn shell_gc_roots_dir(&self) -> PathBuf {
        self.state_dir.join("gcroots").join("shells")
    }

    fn shell_gc_root_session_dir(&self, session: &str) -> PathBuf {
        self.shell_gc_roots_dir().join(session)
    }

    fn lease_dir(&self) -> PathBuf {
        self.state_dir.join("leases")
    }

    fn lease_path(&self, client_id: &str) -> PathBuf {
        self.lease_dir().join(format!("{client_id}.json"))
    }

    fn holders_dir(&self, session: &str) -> PathBuf {
        self.shell_gc_root_session_dir(session).join("holders")
    }

    fn gc_state(&self, protected_session: Option<&str>) {
        let live_sessions = self.gc_shell_roots(protected_session);
        self.gc_snapshots(&live_sessions);
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

    fn gc_shell_roots(&self, protected_session: Option<&str>) -> HashSet<String> {
        let mut live_sessions = HashSet::new();
        let protected_session = protected_session.filter(|session| is_valid_session(session));
        if let Some(session) = protected_session {
            live_sessions.insert(session.to_string());
        }
        let max_age = shell_gc_root_ttl();
        let Ok(entries) = std::fs::read_dir(self.shell_gc_roots_dir()) else {
            return live_sessions;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(session) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if protected_session == Some(session) {
                continue;
            }
            let live = self.session_has_live_holder(session);
            if live {
                live_sessions.insert(session.to_string());
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
        live_sessions
    }

    fn session_has_live_holder(&self, session: &str) -> bool {
        let holders_dir = self.holders_dir(session);
        let Ok(entries) = std::fs::read_dir(&holders_dir) else {
            return false;
        };

        let mut live = false;
        for entry in entries.flatten() {
            let path = entry.path();
            // Skip a writer's in-flight `atomic_write` temp so the scan can't
            // delete it out from under the pending rename.
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let holder = std::fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<SessionHolder>(&raw).ok());
            match holder {
                Some(holder) if self.session_holder_is_live(&holder) => live = true,
                _ => {
                    std::fs::remove_file(path).ok();
                }
            }
        }
        live
    }

    fn session_holder_is_live(&self, holder: &SessionHolder) -> bool {
        match holder {
            SessionHolder::Lease { client_id } => self
                .read_lease_record(client_id)
                .map(|lease| lease_record_is_live(&lease))
                .unwrap_or(false),
            SessionHolder::Process {
                pid, start_time, ..
            } => process_holder_is_live(*pid, start_time),
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

    fn write_session_holder(&self, session: &str, holder: &SessionHolder) -> Result<()> {
        if !is_valid_session(session) {
            bail!("invalid cade session id")
        }
        if !self.touch_shell_gc_session(session) {
            bail!("cannot refresh cade session holder")
        }
        let holders_dir = self.holders_dir(session);
        std::fs::create_dir_all(&holders_dir).context("create cade session holders dir")?;
        let path = holders_dir.join(holder_file_name(holder)?);
        let body = serde_json::to_vec(holder).context("serialise cade session holder")?;
        atomic_write(&path, &body).context("write cade session holder")
    }

    fn remove_session_holder(&self, session: &str, holder_name: &str) {
        if !is_valid_session(session) {
            return;
        }
        std::fs::remove_file(self.holders_dir(session).join(holder_name)).ok();
        self.touch_shell_gc_session(session);
    }

    fn refresh_process_holder(&self, session: &str, owner_pid: Option<u32>) -> Result<()> {
        let Some(pid) = owner_pid.or_else(parent_pid) else {
            return Ok(());
        };
        let Some(start_time) = process_start_time(pid) else {
            return Ok(());
        };
        self.write_session_holder(
            session,
            &SessionHolder::Process {
                pid,
                start_time,
                last_seen: now_secs(),
            },
        )
    }

    fn refresh_session_holders(
        &self,
        session: &str,
        client_id: Option<&str>,
        owner_pid: Option<u32>,
    ) {
        if let Err(e) = self.refresh_process_holder(session, owner_pid) {
            verbosity::log(
                Verbosity::Trace,
                format_args!("cade: cannot refresh process gc holder: {e}."),
            );
        }
        if let Some(client_id) = configured_client_id(client_id) {
            // an expired or closed lease is a benign, expected state here; never abort activation.
            let result = self
                .read_lease_record(&client_id)
                .and_then(|lease| self.write_session_holder(session, &lease.session_holder()));
            if let Err(e) = result {
                verbosity::log(
                    Verbosity::Trace,
                    format_args!("cade: cannot refresh lease gc holder for {client_id}: {e}."),
                );
            }
        }
    }

    fn read_lease_record(&self, client_id: &str) -> Result<LeaseRecord> {
        validate_client_id(client_id)?;
        let raw = std::fs::read_to_string(self.lease_path(client_id))
            .with_context(|| format!("reading cade lease {client_id}"))?;
        let lease: LeaseRecord = serde_json::from_str(&raw).context("parse cade lease")?;
        if lease.client_id != client_id {
            bail!("cade lease {client_id} has mismatched client id");
        }
        Ok(lease)
    }

    fn write_lease_record(&self, lease: &LeaseRecord) -> Result<()> {
        validate_client_id(&lease.client_id)?;
        std::fs::create_dir_all(self.lease_dir()).context("create cade leases dir")?;
        let body = serde_json::to_vec(lease).context("serialise cade lease")?;
        atomic_write(&self.lease_path(&lease.client_id), &body).context("write cade lease")
    }

    fn refresh_lease_record(
        &self,
        client_id: &str,
        ttl_seconds: Option<u64>,
    ) -> Result<LeaseRecord> {
        let existing = self.read_lease_record(client_id)?;
        let ttl = ttl_seconds
            .map(Duration::from_secs)
            .unwrap_or_else(shell_gc_root_ttl);
        let lease = LeaseRecord {
            client_id: existing.client_id,
            kind: existing.kind,
            project: existing.project,
            expires_at: now_secs().saturating_add(ttl.as_secs()),
            last_seen: now_secs(),
        };
        self.write_lease_record(&lease)?;
        Ok(lease)
    }

    fn remove_current_session_holders(
        &self,
        session: &str,
        client_id: Option<&str>,
        owner_pid: Option<u32>,
    ) {
        if let Some(pid) = owner_pid.or_else(parent_pid)
            && let Some(start_time) = process_start_time(pid)
        {
            self.remove_session_holder(
                session,
                &holder_file_name(&SessionHolder::Process {
                    pid,
                    start_time,
                    last_seen: now_secs(),
                })
                .unwrap_or_default(),
            );
        }

        if let Some(client_id) = configured_client_id(client_id)
            && is_valid_client_id(&client_id)
        {
            self.remove_session_holder(session, &format!("lease-{client_id}.json"));
        }
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
        let profiles_dir = self.shell_gc_root_session_dir(session).join("profiles");
        if let Err(e) = std::fs::create_dir_all(&profiles_dir) {
            verbosity::log(
                Verbosity::Trace,
                format_args!(
                    "cade: cannot create nix profiles dir at {}: {e}.",
                    profiles_dir.display()
                ),
            );
            return None;
        }
        let key = stable_hash_hex(&format!("{}:{spec}", path.display()));
        Some(profiles_dir.join(format!("{layer_count}-{action_index}-{key}")))
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
        // participants may be non-contiguous, so gap-fill walks them, not raw parents
        let chain = participant_dirs(&root);
        if chain.is_empty() {
            return Ok(());
        }
        // fill from the tip up to the nearest already-approved participant
        let mut base = None;
        for (i, dir) in chain.iter().enumerate() {
            if self.get_permission(dir)? {
                base = Some(i);
                break;
            }
        }
        let upto = base.unwrap_or(1);
        for dir in &chain[0..upto] {
            self.record_permission(dir, true)?;
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

    pub fn lease_open(
        &self,
        kind: &str,
        project: Option<&Path>,
        ttl_seconds: Option<u64>,
    ) -> Result<()> {
        if kind.is_empty()
            || !kind
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            bail!("lease kind must contain only letters, digits, '-' or '_'")
        }
        let ttl = ttl_seconds
            .map(Duration::from_secs)
            .unwrap_or_else(shell_gc_root_ttl);
        let lease = LeaseRecord {
            client_id: new_client_id(),
            kind: kind.to_string(),
            project: project.map(|p| p.to_string_lossy().to_string()),
            expires_at: now_secs().saturating_add(ttl.as_secs()),
            last_seen: now_secs(),
        };
        self.write_lease_record(&lease)?;
        let response = LeaseResponse::from(&lease);
        println!("{}", serde_json::to_string(&response)?);
        Ok(())
    }

    pub fn lease_refresh(&self, client_id: &str, ttl_seconds: Option<u64>) -> Result<()> {
        let lease = self.refresh_lease_record(client_id, ttl_seconds)?;
        let response = LeaseResponse::from(&lease);
        println!("{}", serde_json::to_string(&response)?);
        Ok(())
    }

    pub fn lease_close(&self, client_id: &str) -> Result<()> {
        validate_client_id(client_id)?;
        std::fs::remove_file(self.lease_path(client_id)).ok();
        if let Ok(entries) = std::fs::read_dir(self.shell_gc_roots_dir()) {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(session) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                self.remove_session_holder(session, &format!("lease-{client_id}.json"));
            }
        }
        Ok(())
    }

    pub fn do_activation(
        &mut self,
        shell: &dyn ShellOutput,
        announce: Option<Announce>,
        client_id: Option<&str>,
        owner_pid: Option<u32>,
    ) -> Result<()> {
        let root_hint = find_cade_root(&self.cwd).unwrap_or_else(|| self.cwd.clone());
        let spinner = crate::progress::start(&root_hint.display().to_string());

        let (activation_env, session, new_session) = self.activation_env_with_snapshot()?;
        let plan = self.activation_plan(Some(&session))?;
        self.refresh_session_holders(&session, client_id, owner_pid);
        clear_disallowed_root_marker(shell);
        let rollup = &plan.rollup;

        for hook in &rollup.hooks {
            if hook.kind == HookType::LoadPre {
                log_hook(hook);
                print!("{}", shell.emit_hook(&hook.content));
            }
        }

        self.root_nix_store_paths(&session, &plan.nix_store_paths);
        if new_session {
            print!("{}", shell.set_env("__CADE_SESSION", &session));
        }

        let delta = rollup.env_delta(&activation_env);
        print!("{}", delta.render_shell(shell));

        for hook in &rollup.hooks {
            if hook.kind == HookType::LoadPost {
                log_hook(hook);
                print!("{}", shell.emit_hook(&hook.content));
            }
        }

        let layer_paths: Vec<String> = plan
            .cade_files
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

        let watch_state = build_watch_state(&plan.root, layer_paths.clone(), &plan.all_watch_files);
        let watches_json = serde_json::to_string(&watch_state).unwrap_or_default();
        print!("{}", shell.set_env("__CADE_WATCHES", &watches_json));

        match announce {
            Some(announce) => spinner.success(&format!(
                "cade: {} {}{}.",
                announce.verb(),
                plan.root.display(),
                layer_count_suffix(layer_paths.len())
            )),
            None => spinner.done(),
        }
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
        shell: &dyn ShellOutput,
        finalise: bool,
        announce: bool,
        client_id: Option<&str>,
        owner_pid: Option<u32>,
    ) -> Result<()> {
        let layers = std::env::var("__CADE_LAYERS").ok();
        let session = std::env::var("__CADE_SESSION").ok();

        if layers.is_none() && session.is_none() && std::env::var("__CADE_SET").is_err() {
            return Ok(());
        }

        let prev_env: HashMap<String, String> = session
            .as_deref()
            .and_then(|s| self.read_snapshot(s))
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

        if announce && let Some(layers) = &layers {
            let paths: Vec<&str> = layers.split('\x1F').filter(|s| !s.is_empty()).collect();
            if let Some(tip) = paths.last() {
                announce_unloaded(tip, paths.len());
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
            if let Some(session) = session.as_deref() {
                self.remove_current_session_holders(session, client_id, owner_pid);
            }
            self.gc_state(session.as_deref());
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

    pub fn do_reload(
        &mut self,
        shell: &dyn ShellOutput,
        client_id: Option<&str>,
        owner_pid: Option<u32>,
    ) -> Result<()> {
        let cwd = self.cwd.clone();
        let (active, disallowed_tip) = self.resolve_active(&cwd)?;
        let new_root = active.first().cloned();
        let new_set: BTreeSet<String> = active
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let is_active = std::env::var("__CADE_LAYERS").is_ok();

        // not active yet
        if !is_active {
            if new_root.is_some() {
                self.do_activation(shell, Some(Announce::Loaded), client_id, owner_pid)?;
                self.sync_disallowed_prompt(disallowed_tip.as_deref(), shell);
            } else {
                self.sync_disallowed_prompt(disallowed_tip.as_deref(), shell);
            }
            return Ok(());
        }

        if let Ok(session) = std::env::var("__CADE_SESSION")
            && is_valid_session(&session)
        {
            self.refresh_session_holders(&session, client_id, owner_pid);
        }

        let state = std::env::var("__CADE_WATCHES")
            .ok()
            .and_then(|w| serde_json::from_str::<WatchState>(&w).ok());
        let old_set: BTreeSet<String> = state
            .as_ref()
            .map(|s| s.cade_paths.iter().cloned().collect())
            .unwrap_or_default();
        let old_root = state.as_ref().map(|s| s.root.clone());
        let files_stale = state.as_ref().map(files_changed).unwrap_or(true);

        // unchanged; only keep the disallowed-child prompt in sync
        if new_set == old_set && !files_stale {
            self.sync_disallowed_prompt(disallowed_tip.as_deref(), shell);
            return Ok(());
        }

        match &new_root {
            // left every approved layer
            None => {
                self.do_restore(shell, true, true, client_id, owner_pid)?;
            }
            Some(new_root) => {
                // The transition is two orthogonal decisions, not four topologies:
                //   - the old tip leaving (restore announces it) iff it is no
                //     longer in the new set;
                //   - the reactivation verb: reloaded if the root is unchanged,
                //     silent if we only dropped to a tip that already composed,
                //     loaded otherwise (a genuinely new tip).
                // The parents in between join/leave via the set-difference loop;
                // it skips the two tips, which the restore and activation own.
                let new_tip = new_root.to_string_lossy().to_string();
                let old_tip = old_root.as_deref();

                let unload_old_tip = old_tip.is_none_or(|t| !new_set.contains(t));
                let verb = if old_tip == Some(new_tip.as_str()) {
                    Some(Announce::Reloaded)
                } else if old_set.contains(&new_tip) {
                    None // dropped to a tip that already composed
                } else {
                    Some(Announce::Loaded)
                };

                self.do_restore(shell, false, unload_old_tip, client_id, owner_pid)?;
                for dir in old_set.difference(&new_set) {
                    if Some(dir.as_str()) != old_tip {
                        announce_unloaded(dir, 1);
                    }
                }
                for dir in new_set.difference(&old_set) {
                    if *dir != new_tip {
                        announce_loaded(dir);
                    }
                }
                self.do_activation(shell, verb, client_id, owner_pid)?;
            }
        }
        self.sync_disallowed_prompt(disallowed_tip.as_deref(), shell);
        Ok(())
    }

    /// prompt to allow a disallowed tip, or clear a stale prompt; leaves the parent env alone
    fn sync_disallowed_prompt(&self, disallowed_tip: Option<&Path>, shell: &dyn ShellOutput) {
        match disallowed_tip {
            Some(tip) => mark_disallowed_root(tip, shell),
            None => clear_disallowed_root_marker(shell),
        }
    }

    /// The anchored-permission rule, owned in one place: from a tip-first
    /// participant list, keep the contiguous approved run anchored on the
    /// deepest approved participant (a disallowed tip is skipped, not refused)
    /// and cap at the first unapproved dir above it. Stays tip-first.
    fn approved_participants(&mut self, participants: &[PathBuf]) -> Result<Vec<PathBuf>> {
        let mut active = Vec::new();
        let mut anchored = false;
        for p in participants {
            if self.get_permission(p)? {
                anchored = true;
                active.push(p.clone());
            } else if anchored {
                break;
            }
        }
        Ok(active)
    }

    /// layers to compose, parent-first; anchors on the deepest approved participant
    /// (a disallowed tip is skipped, not refused) and caps at the first unapproved above it
    fn approved_chain(&mut self, root: &Path) -> Result<Vec<(PathBuf, Vec<Keyword>)>> {
        let approved = self.approved_participants(&participant_dirs(root))?;
        let mut chain = Vec::with_capacity(approved.len());
        for path in approved {
            let keywords = config_keywords(&path)?;
            chain.push((path, keywords));
        }
        chain.reverse(); // parent-first for rollup
        Ok(chain)
    }

    /// participants that will compose at `cwd`, tip-first (anchored on the deepest
    /// approved), plus the deepest participant if it is disallowed (to prompt for it)
    fn resolve_active(&mut self, cwd: &Path) -> Result<(Vec<PathBuf>, Option<PathBuf>)> {
        let participants = participant_dirs(cwd);
        let active = self.approved_participants(&participants)?;
        let disallowed_tip = match participants.first() {
            Some(tip) if active.first() != Some(tip) => Some(tip.clone()),
            _ => None,
        };
        Ok((active, disallowed_tip))
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
                for dir in participant_dirs(r) {
                    let allowed = self.get_permission(&dir)?;
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
                    println!("  {}  [{mark}]", dir.display());
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
    session: Option<&str>,
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
                    let profile = session.and_then(|session| {
                        cade.nix_profile_path(session, layer_count, action_index, path, "flake")
                    });
                    load_flake(path, None, profile).context("loading flake")
                }
                Flake(output) => {
                    let profile = session.and_then(|session| {
                        cade.nix_profile_path(
                            session,
                            layer_count,
                            action_index,
                            path,
                            &format!("flake:{output}"),
                        )
                    });
                    load_flake(path, Some(output.clone()), profile).context("loading flake")
                }
                Shell(filename) => {
                    let profile = session.and_then(|session| {
                        cade.nix_profile_path(
                            session,
                            layer_count,
                            action_index,
                            path,
                            &format!("shell:{filename}"),
                        )
                    });
                    load_shell(path, filename.clone(), profile).context("loading shell")
                }
                Env(filename) => load_env(path, filename.clone()).context("loading env file"),
                Envrc(filename) => {
                    let profile_dir = session.and_then(|session| {
                        cade.nix_profile_path(
                            session,
                            layer_count,
                            action_index,
                            path,
                            &format!("envrc:{filename}"),
                        )
                    });
                    crate::envrc::load_envrc(path, filename.clone(), profile_dir)
                        .context("loading .envrc")
                }
            }
            .map(CadeAction::Environ),
            Hook(hook) => Ok(CadeAction::Hook(hook.clone())),
            Clear(vars) => Ok(CadeAction::Clear(vars.clone())),
            Concat(vars) => Ok(CadeAction::Concat(vars.clone())),
            Set(env) => Ok(CadeAction::Environ(env.clone())),
            // affects only chain construction, not the loaded environment
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

fn build_watch_state(
    root: &Path,
    cade_paths: Vec<String>,
    watched_files: &[PathBuf],
) -> WatchState {
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

/// any watched file changed (mtime, size, or gone); the layer-set change is checked separately
fn files_changed(state: &WatchState) -> bool {
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
