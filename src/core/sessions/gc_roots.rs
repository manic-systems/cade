use std::{
    collections::HashSet,
    fs::{
        create_dir_all,
        read_dir,
        read_link,
        read_to_string,
        remove_dir_all,
        remove_file,
        write as write_file,
    },
    path::{
        Path,
        PathBuf,
    },
    process::{
        Command,
        Stdio,
    },
};

use anyhow::{
    Context as _,
    Result,
    bail,
};

use crate::{
    core::{
        Cade,
        sessions::{
            SessionHolder,
            identity::{
                atomic_write,
                configured_client_id,
                is_valid_client_id,
                is_valid_session,
                now_secs,
                parent_pid,
                process_holder_is_live,
                process_start_time,
                stable_hash_hex,
            },
            leases::{
                lease_record_is_live,
                read_lease_record,
            },
            shell_gc_root_ttl,
        },
    },
    verbosity::{
        self,
        Verbosity,
    },
};

fn rooted_store_paths(session_dir: &Path) -> HashSet<String> {
    let mut rooted = HashSet::new();
    let Ok(entries) = read_dir(session_dir) else {
        return rooted;
    };
    for entry in entries.flatten() {
        let is_symlink = entry
            .file_type()
            .is_ok_and(|file_type| file_type.is_symlink());
        if !is_symlink {
            continue;
        }
        if let Ok(link) = read_link(entry.path())
            && let Some(link_text) = link.to_str()
        {
            rooted.insert(link_text.to_owned());
        }
    }
    rooted
}

pub(super) fn shell_gc_roots_dir(cade: &Cade) -> PathBuf {
    cade.state_dir.join("gcroots").join("shells")
}

fn shell_gc_root_session_dir(cade: &Cade, session: &str) -> PathBuf {
    shell_gc_roots_dir(cade).join(session)
}

fn holders_dir(cade: &Cade, session: &str) -> PathBuf {
    shell_gc_root_session_dir(cade, session).join("holders")
}

pub fn gc_state(cade: &Cade, protected_session: Option<&str>) {
    let live_sessions = gc_shell_roots(cade, protected_session);
    gc_session_files(&cade.state_dir.join("snapshots"), &live_sessions, |name| {
        name.strip_suffix(".env")
    });
    gc_session_files(&cade.state_dir.join("watches"), &live_sessions, |name| {
        name.strip_suffix(".json")?
            .rsplit_once('-')
            .map(|(session, _)| session)
    });
}

fn gc_session_files(
    dir: &Path,
    live_sessions: &HashSet<String>,
    session_of: fn(&str) -> Option<&str>,
) {
    let max_age = shell_gc_root_ttl();
    let Ok(entries) = read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let active = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(session_of)
            .is_some_and(|session| live_sessions.contains(session));
        if active {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|stamp| stamp.elapsed().is_ok_and(|age| age > max_age));
        if stale {
            let _ = remove_file(path);
        }
    }
}

fn gc_shell_roots(cade: &Cade, protected_session: Option<&str>) -> HashSet<String> {
    let mut live_sessions = HashSet::new();
    let checked_protected = protected_session.filter(|session| is_valid_session(session));
    if let Some(session) = checked_protected {
        live_sessions.insert(session.to_owned());
    }
    let max_age = shell_gc_root_ttl();
    let Ok(entries) = read_dir(shell_gc_roots_dir(cade)) else {
        return live_sessions;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(session) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if checked_protected == Some(session) {
            continue;
        }
        let live = session_has_live_holder(cade, session);
        if live {
            live_sessions.insert(session.to_owned());
            continue;
        }
        let marker = path.join(".last-used");
        let stale = marker
            .metadata()
            .or_else(|_| entry.metadata())
            .and_then(|meta| meta.modified())
            .is_ok_and(|stamp| stamp.elapsed().is_ok_and(|age| age > max_age));
        if stale {
            let _ = remove_dir_all(path);
        }
    }
    live_sessions
}

fn session_has_live_holder(cade: &Cade, session: &str) -> bool {
    let holders_dir = holders_dir(cade, session);
    let Ok(entries) = read_dir(&holders_dir) else {
        return false;
    };

    let mut live = false;
    for entry in entries.flatten() {
        let path = entry.path();

        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let holder = read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<SessionHolder>(&raw).ok());
        match holder {
            Some(candidate) if session_holder_is_live(cade, &candidate) => live = true,
            _ => {
                let _ = remove_file(path);
            },
        }
    }
    live
}

fn session_holder_is_live(cade: &Cade, holder: &SessionHolder) -> bool {
    match *holder {
        SessionHolder::Lease { ref client_id } => {
            read_lease_record(cade, client_id).is_ok_and(|lease| lease_record_is_live(&lease))
        },
        SessionHolder::Process {
            ref pid,
            ref start_time,
            ..
        } => process_holder_is_live(*pid, start_time),
    }
}

fn touch_shell_gc_session(cade: &Cade, session: &str) -> bool {
    if !is_valid_session(session) {
        return false;
    }
    let session_dir = shell_gc_root_session_dir(cade, session);
    if let Err(io_err) = create_dir_all(&session_dir) {
        verbosity::log(
            Verbosity::Normal,
            format_args!(
                "cade: cannot create nix gc root dir at {}: {io_err}.",
                session_dir.display()
            ),
        );
        return false;
    }
    if let Err(marker_err) = write_file(session_dir.join(".last-used"), b"") {
        verbosity::log(
            Verbosity::Normal,
            format_args!(
                "cade: cannot refresh nix gc root marker at {}: {marker_err}.",
                session_dir.display()
            ),
        );
        return false;
    }
    true
}

fn write_session_holder(cade: &Cade, session: &str, holder: &SessionHolder) -> Result<()> {
    if !is_valid_session(session) {
        bail!("invalid cade session id")
    }
    if !touch_shell_gc_session(cade, session) {
        bail!("cannot refresh cade session holder")
    }
    let holders_dir = holders_dir(cade, session);
    create_dir_all(&holders_dir).context("create cade session holders dir")?;
    let path = holders_dir.join(holder.file_name()?);
    let body = serde_json::to_vec(holder).context("serialise cade session holder")?;
    atomic_write(&path, &body).context("write cade session holder")
}

pub(super) fn remove_session_holder(cade: &Cade, session: &str, holder: &SessionHolder) {
    if !is_valid_session(session) {
        return;
    }
    let Ok(holder_name) = holder.file_name() else {
        return;
    };
    let _ = remove_file(holders_dir(cade, session).join(holder_name));
    touch_shell_gc_session(cade, session);
}

fn refresh_process_holder(cade: &Cade, session: &str, owner_pid: Option<u32>) -> Result<()> {
    let Some(pid) = owner_pid.or_else(parent_pid) else {
        return Ok(());
    };
    let Some(start_time) = process_start_time(pid) else {
        return Ok(());
    };
    write_session_holder(
        cade,
        session,
        &SessionHolder::process(pid, start_time, now_secs()),
    )
}

pub fn refresh_session_holders(
    cade: &Cade,
    session: &str,
    client_id: Option<&str>,
    owner_pid: Option<u32>,
) {
    if let Err(refresh_err) = refresh_process_holder(cade, session, owner_pid) {
        verbosity::log(
            Verbosity::Trace,
            format_args!("cade: cannot refresh process gc holder: {refresh_err}."),
        );
    }
    if let Some(resolved_id) = configured_client_id(client_id) {
        let result = read_lease_record(cade, &resolved_id)
            .and_then(|lease| write_session_holder(cade, session, &lease.session_holder()));
        if let Err(write_err) = result {
            verbosity::log(
                Verbosity::Trace,
                format_args!(
                    "cade: cannot refresh lease gc holder for {resolved_id}: {write_err}."
                ),
            );
        }
    }
}

pub fn remove_current_session_holders(
    cade: &Cade,
    session: &str,
    client_id: Option<&str>,
    owner_pid: Option<u32>,
) {
    if let Some(pid) = owner_pid.or_else(parent_pid)
        && let Some(start_time) = process_start_time(pid)
    {
        remove_session_holder(
            cade,
            session,
            &SessionHolder::process(pid, start_time, now_secs()),
        );
    }

    if let Some(resolved_id) = configured_client_id(client_id)
        && is_valid_client_id(&resolved_id)
    {
        remove_session_holder(cade, session, &SessionHolder::lease(resolved_id));
    }
}

pub fn nix_profile_path(
    cade: &Cade,
    session: &str,
    layer_count: usize,
    action_index: usize,
    path: &Path,
    spec: &str,
) -> Option<PathBuf> {
    if !touch_shell_gc_session(cade, session) {
        return None;
    }
    let profiles_dir = shell_gc_root_session_dir(cade, session).join("profiles");
    if let Err(io_err) = create_dir_all(&profiles_dir) {
        verbosity::log(
            Verbosity::Trace,
            format_args!(
                "cade: cannot create nix profiles dir at {}: {io_err}.",
                profiles_dir.display()
            ),
        );
        return None;
    }
    let key = stable_hash_hex(&format!("{}:{spec}", path.display()));
    Some(profiles_dir.join(format!("{layer_count}-{action_index}-{key}")))
}

pub fn root_nix_store_paths(cade: &Cade, session: &str, paths: &[String]) {
    if paths.is_empty() || !is_valid_session(session) {
        return;
    }

    if !touch_shell_gc_session(cade, session) {
        return;
    }
    let session_dir = shell_gc_root_session_dir(cade, session);

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
    let add_result = Command::new("nix-store")
        .args(["--add-root"])
        .arg(&base)
        .args(["--indirect", "-r"])
        .args(&to_root)
        .stdout(Stdio::null())
        .status();
    match add_result {
        Ok(status) if status.success() => {},
        Ok(status) => {
            verbosity::log(
                Verbosity::Normal,
                format_args!(
                    "cade: nix-store failed to add {} gc root(s) ({status}).",
                    to_root.len()
                ),
            );
        },
        Err(spawn_err) => {
            verbosity::log(
                Verbosity::Normal,
                format_args!("cade: failed to add nix gc roots: {spawn_err}."),
            );
        },
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)] use std::os::unix::fs::symlink;
    use std::{
        env::temp_dir,
        fs::{
            File,
            create_dir_all,
            remove_dir_all,
            write as write_file,
        },
        process::id as process_id,
        thread::current as current_thread,
        time::{
            Duration,
            SystemTime,
        },
    };

    use crate::core::{
        Cade,
        sessions::gc_roots::{
            gc_state,
            rooted_store_paths,
        },
    };

    #[test]
    fn gc_state_removes_stale_watch_files_for_dead_sessions_only() {
        let state_dir = temp_dir().join(format!(
            "cade-gc-watches-{}-{}",
            process_id(),
            current_thread().name().unwrap_or("test")
        ));
        let watches = state_dir.join("watches");
        let _ = remove_dir_all(&state_dir);
        create_dir_all(&watches).unwrap();
        let dead = watches.join("deadsession-0123456789abcdef.json");
        let live = watches.join("livesession-0123456789abcdef.json");
        let long_ago = SystemTime::now() - Duration::from_hours(9600);
        for path in [&dead, &live] {
            write_file(path, b"{}").unwrap();
            File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(long_ago)
                .unwrap();
        }
        let cade = Cade {
            db:        rusqlite::Connection::open_in_memory().unwrap(),
            cwd:       state_dir.clone(),
            state_dir: state_dir.clone(),
        };

        gc_state(&cade, Some("livesession"));

        assert!(!dead.exists());
        assert!(live.exists());
        let _ = remove_dir_all(&state_dir);
    }

    #[cfg(unix)]
    #[test]
    fn rooted_store_paths_collects_symlink_targets_only() {
        let dir = temp_dir().join(format!("cade-rooted-{}", process_id()));
        create_dir_all(&dir).unwrap();
        let target = format!("/nix/store/{}-pkg", "a".repeat(32));
        symlink(&target, dir.join("cade-deadbeef")).unwrap();
        write_file(dir.join(".last-used"), b"").unwrap();
        create_dir_all(dir.join("profiles")).unwrap();

        let rooted = rooted_store_paths(&dir);
        assert!(rooted.contains(&target));
        assert_eq!(rooted.len(), 1);

        let _ = remove_dir_all(&dir);
    }
}
