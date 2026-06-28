use super::{
    SessionHolder,
    identity::{
        atomic_write, configured_client_id, is_valid_client_id, is_valid_session, now_secs,
        parent_pid, process_holder_is_live, process_start_time, stable_hash_hex,
    },
    leases::lease_record_is_live,
    shell_gc_root_ttl,
};
use crate::core::Cade;
use crate::verbosity::{self, Verbosity};
use anyhow::{Context, Result, bail};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::Command,
};

fn rooted_store_paths(session_dir: &Path) -> HashSet<String> {
    let mut rooted = HashSet::new();
    let Ok(entries) = std::fs::read_dir(session_dir) else {
        return rooted;
    };
    for entry in entries.flatten() {
        let is_symlink = entry.file_type().map(|t| t.is_symlink()).unwrap_or(false);
        if !is_symlink {
            continue;
        }
        if let Ok(target) = std::fs::read_link(entry.path())
            && let Some(target) = target.to_str()
        {
            rooted.insert(target.to_string());
        }
    }
    rooted
}

impl Cade {
    pub(super) fn shell_gc_roots_dir(&self) -> PathBuf {
        self.state_dir.join("gcroots").join("shells")
    }

    fn shell_gc_root_session_dir(&self, session: &str) -> PathBuf {
        self.shell_gc_roots_dir().join(session)
    }

    fn holders_dir(&self, session: &str) -> PathBuf {
        self.shell_gc_root_session_dir(session).join("holders")
    }

    pub(in crate::core) fn gc_state(&self, protected_session: Option<&str>) {
        let live_sessions = self.gc_shell_roots(protected_session);
        self.gc_session_files(&self.state_dir.join("snapshots"), &live_sessions, |name| {
            name.strip_suffix(".env")
        });
        self.gc_session_files(&self.state_dir.join("watches"), &live_sessions, |name| {
            name.strip_suffix(".json")?.rsplit_once('-').map(|(s, _)| s)
        });
    }

    fn gc_session_files(
        &self,
        dir: &Path,
        live_sessions: &HashSet<String>,
        session_of: fn(&str) -> Option<&str>,
    ) {
        let max_age = shell_gc_root_ttl();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let active = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(session_of)
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
        let path = holders_dir.join(holder.file_name()?);
        let body = serde_json::to_vec(holder).context("serialise cade session holder")?;
        atomic_write(&path, &body).context("write cade session holder")
    }

    pub(super) fn remove_session_holder(&self, session: &str, holder: &SessionHolder) {
        if !is_valid_session(session) {
            return;
        }
        let Ok(holder_name) = holder.file_name() else {
            return;
        };
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
            &SessionHolder::process(pid, start_time, now_secs()),
        )
    }

    pub(in crate::core) fn refresh_session_holders(
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

    pub(in crate::core) fn remove_current_session_holders(
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
                &SessionHolder::process(pid, start_time, now_secs()),
            );
        }

        if let Some(client_id) = configured_client_id(client_id)
            && is_valid_client_id(&client_id)
        {
            self.remove_session_holder(session, &SessionHolder::lease(client_id));
        }
    }

    pub(in crate::core) fn nix_profile_path(
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

    pub(in crate::core) fn root_nix_store_paths(&self, session: &str, paths: &[String]) {
        if paths.is_empty() || !is_valid_session(session) {
            return;
        }

        if !self.touch_shell_gc_session(session) {
            return;
        }
        let session_dir = self.shell_gc_root_session_dir(session);

        let already_rooted = rooted_store_paths(&session_dir);

        let mut seen = HashSet::new();
        let mut to_root: Vec<String> = Vec::new();
        for store_path in paths {
            if !seen.insert(store_path.as_str()) || already_rooted.contains(store_path) {
                continue;
            }
            if !Path::new(store_path).exists() {
                verbosity::log(
                    Verbosity::Trace,
                    format_args!("cade: skipping missing nix store path {store_path}."),
                );
                continue;
            }
            to_root.push(store_path.clone());
        }
        if to_root.is_empty() {
            return;
        }
        to_root.sort_unstable();

        let base = session_dir.join(format!("cade-{}", stable_hash_hex(&to_root.join("\n"))));
        let status = Command::new("nix-store")
            .args(["--add-root"])
            .arg(&base)
            .args(["--indirect", "-r"])
            .args(&to_root)
            .stdout(std::process::Stdio::null())
            .status();
        match status {
            Ok(status) if status.success() => {}
            Ok(status) => verbosity::log(
                Verbosity::Normal,
                format_args!(
                    "cade: nix-store failed to add {} gc root(s) ({status}).",
                    to_root.len()
                ),
            ),
            Err(e) => verbosity::log(
                Verbosity::Normal,
                format_args!("cade: failed to add nix gc roots: {e}."),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gc_state_removes_stale_watch_files_for_dead_sessions_only() {
        let state_dir = std::env::temp_dir().join(format!(
            "cade-gc-watches-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let watches = state_dir.join("watches");
        std::fs::remove_dir_all(&state_dir).ok();
        std::fs::create_dir_all(&watches).unwrap();
        let dead = watches.join("deadsession-0123456789abcdef.json");
        let live = watches.join("livesession-0123456789abcdef.json");
        let long_ago =
            std::time::SystemTime::now() - std::time::Duration::from_secs(400 * 24 * 3600);
        for path in [&dead, &live] {
            std::fs::write(path, b"{}").unwrap();
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(long_ago)
                .unwrap();
        }
        let cade = Cade {
            db: rusqlite::Connection::open_in_memory().unwrap(),
            cwd: state_dir.clone(),
            state_dir: state_dir.clone(),
        };

        cade.gc_state(Some("livesession"));

        assert!(!dead.exists());
        assert!(live.exists());
        std::fs::remove_dir_all(&state_dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn rooted_store_paths_collects_symlink_targets_only() {
        let dir = std::env::temp_dir().join(format!("cade-rooted-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = format!("/nix/store/{}-pkg", "a".repeat(32));
        std::os::unix::fs::symlink(&target, dir.join("cade-deadbeef")).unwrap();
        std::fs::write(dir.join(".last-used"), b"").unwrap();
        std::fs::create_dir_all(dir.join("profiles")).unwrap();

        let rooted = rooted_store_paths(&dir);
        assert!(rooted.contains(&target));
        assert_eq!(rooted.len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }
}
