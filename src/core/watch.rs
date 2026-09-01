use super::{
    Cade,
    layer::tokenize_args,
    sessions::{atomic_write, is_valid_session, stable_hash_hex},
};
use crate::types::Keyword;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
};

pub(super) const LAYER_CACHE_VERSION: &str = "layer-cache-v4";

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct WatchEntry {
    path: PathBuf,
    state: WatchFileState,
}

impl WatchEntry {
    fn capture(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            state: watch_file_state(path),
        }
    }

    fn changed(&self) -> bool {
        self.state.changed(&self.path)
    }

    fn token_part(&self) -> String {
        match &self.state {
            WatchFileState::Present {
                mtime,
                size,
                content_hash,
            } => match content_hash {
                Some(content_hash) => {
                    format!("{}:present:{size}:{content_hash:016x}", self.path.display())
                }
                None => format!("{}:present-unreadable:{mtime}:{size}", self.path.display()),
            },
            WatchFileState::Missing => format!("{}:missing", self.path.display()),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum WatchFileState {
    Present {
        mtime: u64,
        size: u64,
        content_hash: Option<u64>,
    },
    Missing,
}

impl WatchFileState {
    fn changed(&self, path: &Path) -> bool {
        let Ok(meta) = std::fs::metadata(path) else {
            return *self != WatchFileState::Missing;
        };
        let current_mtime = mtime_nanos(&meta);
        let current_size = meta.len();

        match self {
            WatchFileState::Missing => true,
            WatchFileState::Present {
                mtime,
                size,
                content_hash,
            } => {
                if *mtime == current_mtime && *size == current_size {
                    return false;
                }
                if *size != current_size {
                    return true;
                }
                match content_hash {
                    Some(expected) => content_hash_for(path) != Some(*expected),
                    None => true,
                }
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct WatchState {
    #[serde(default)]
    version: String,
    root: PathBuf,
    cade_paths: Vec<PathBuf>,
    files: Vec<WatchEntry>,
}

impl WatchState {
    pub(super) fn capture(
        root: &Path,
        cade_paths: Vec<PathBuf>,
        watched_files: &[PathBuf],
    ) -> Self {
        Self {
            version: LAYER_CACHE_VERSION.to_string(),
            root: root.to_path_buf(),
            cade_paths,
            files: watch_entries(watched_files),
        }
    }

    pub(super) fn root_string(&self) -> String {
        self.root.to_string_lossy().to_string()
    }

    pub(super) fn cade_path_set(&self) -> BTreeSet<String> {
        self.cade_paths
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect()
    }

    pub(super) fn files_changed(&self) -> bool {
        self.version != LAYER_CACHE_VERSION || self.files.iter().any(WatchEntry::changed)
    }
}

impl Cade {
    // Named by hash rather than session so a subshell's reload doesn't
    // replace the file its parent still diffs against.
    pub(super) fn persist_watch_state(
        &self,
        session: &str,
        watches: &WatchState,
    ) -> Result<String> {
        if !is_valid_session(session) {
            bail!("invalid cade session id")
        }
        let body = serde_json::to_vec(watches).context("serialize watch state")?;
        let dir = self.state_dir.join("watches");
        std::fs::create_dir_all(&dir).context("create watches dir")?;
        let hash = stable_hash_hex(&String::from_utf8_lossy(&body));
        let path = dir.join(format!("{session}-{hash}.json"));
        atomic_write(&path, &body).context("write watch state")?;
        Ok(path.to_string_lossy().to_string())
    }
}

// Inline json is the pre-file format still living in older shells.
pub fn load_watch_ref(raw: &str) -> Option<WatchState> {
    if raw.starts_with('{') {
        return serde_json::from_str(raw).ok();
    }
    let body = std::fs::read_to_string(raw).ok()?;
    serde_json::from_str(&body).ok()
}

impl Cade {
    // The walk is skipped while every file found last time keeps its mtime and
    // size, so a new file goes unseen until an already-watched one changes.
    pub(super) fn layer_watch(
        &self,
        dir: &Path,
        keywords: &[Keyword],
    ) -> Result<(Vec<PathBuf>, String)> {
        let key = dir.to_string_lossy();
        if let Some((files, token)) = self.get_watch_discovery(&key)
            && compute_layer_key(&files) == token
        {
            return Ok((files, token));
        }

        let files = watched_files_for_keywords(dir, keywords)?;
        let token = compute_layer_key(&files);
        self.store_watch_discovery(&key, &files, &token)?;
        Ok((files, token))
    }
}

fn watched_files_for_keywords(dir: &Path, keywords: &[Keyword]) -> Result<Vec<PathBuf>> {
    let mut files = vec![dir.join(".cade")];
    for kw in keywords {
        match kw {
            Keyword::Load(loadable) => files.extend(loadable.resolve(dir).watch),
            Keyword::Watch(raw) => files.extend(tokenize_args(raw)?.iter().map(|w| dir.join(w))),
            _ => {}
        }
    }
    Ok(files)
}

pub(super) fn compute_layer_key(watched_files: &[PathBuf]) -> String {
    let mut parts = vec![LAYER_CACHE_VERSION.to_string()];
    for entry in watch_entries(watched_files) {
        parts.push(entry.token_part());
    }
    parts.join("\n")
}

fn watch_entries(watched_files: &[PathBuf]) -> Vec<WatchEntry> {
    watched_files
        .iter()
        .map(|path| WatchEntry::capture(path))
        .collect()
}

fn watch_file_state(path: &Path) -> WatchFileState {
    match std::fs::metadata(path) {
        Ok(meta) => WatchFileState::Present {
            mtime: mtime_nanos(&meta),
            size: meta.len(),
            content_hash: content_hash_for(path),
        },
        Err(_) => WatchFileState::Missing,
    }
}

fn content_hash_for(path: &Path) -> Option<u64> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hash = 0xcbf29ce484222325u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            return Some(hash);
        }
        for byte in &buffer[..read] {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
}

fn mtime_nanos(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_discovery_refreshes_only_when_a_watched_file_changes() {
        let root = std::env::temp_dir().join(format!(
            "cade-watch-discovery-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(root.join("nix")).unwrap();
        std::fs::write(root.join(".envrc"), "use flake\n").unwrap();
        std::fs::write(root.join("flake.nix"), "{}\n").unwrap();

        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE WatchDiscovery (
                Dir TEXT PRIMARY KEY,
                Token TEXT NOT NULL,
                Files TEXT NOT NULL,
                LastUsed INTEGER NOT NULL DEFAULT 0
            );",
        )
        .unwrap();
        let cade = Cade {
            db,
            cwd: root.clone(),
            state_dir: root.clone(),
        };
        let keywords = [Keyword::Load(crate::types::Loadable::Envrc(String::new()))];
        let extra = root.join("nix").join("extra.nix");

        let (first, _) = cade.layer_watch(&root, &keywords).unwrap();
        assert!(first.contains(&root.join("flake.nix")));
        assert!(!first.contains(&extra));

        std::fs::write(&extra, "{}\n").unwrap();
        let (reused, _) = cade.layer_watch(&root, &keywords).unwrap();
        assert!(!reused.contains(&extra));

        std::fs::write(root.join("flake.nix"), "{ inputs = {}; }\n").unwrap();
        let (rediscovered, _) = cade.layer_watch(&root, &keywords).unwrap();
        assert!(rediscovered.contains(&extra));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn timestamp_only_changes_do_not_invalidate_content_identity() {
        let root = std::env::temp_dir().join(format!(
            "cade-watch-content-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("flake.nix");
        std::fs::write(&path, "same\n").unwrap();

        let entry = WatchEntry::capture(&path);
        let token = compute_layer_key(std::slice::from_ref(&path));
        let old_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_times(
            std::fs::FileTimes::new().set_modified(old_mtime + std::time::Duration::from_secs(1)),
        )
        .unwrap();

        assert!(!entry.changed());
        assert_eq!(compute_layer_key(std::slice::from_ref(&path)), token);

        std::fs::write(&path, "else\n").unwrap();
        assert!(entry.changed());
        assert_ne!(compute_layer_key(std::slice::from_ref(&path)), token);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn old_watch_state_versions_are_stale() {
        let state = WatchState {
            version: "layer-cache-v2".to_string(),
            root: PathBuf::from("/project"),
            cade_paths: vec![PathBuf::from("/project")],
            files: Vec::new(),
        };

        assert!(state.files_changed());
    }

    #[test]
    fn missing_watch_state_version_is_stale() {
        let raw = r#"{"root":"/project","cade_paths":["/project"],"files":[]}"#;
        let state: WatchState = serde_json::from_str(raw).unwrap();

        assert!(state.files_changed());
    }

    #[test]
    fn watch_ref_stays_short_for_huge_watch_lists() {
        let state_dir = std::env::temp_dir().join(format!("cade-watchref-{}", std::process::id()));
        std::fs::create_dir_all(&state_dir).unwrap();
        let cade = Cade {
            db: rusqlite::Connection::open_in_memory().unwrap(),
            cwd: state_dir.clone(),
            state_dir: state_dir.clone(),
        };
        let files = (0..5000)
            .map(|i| PathBuf::from(format!("/project/third_party/component-{i}/package.json")))
            .collect::<Vec<PathBuf>>();
        let state = WatchState::capture(
            Path::new("/project"),
            vec![PathBuf::from("/project")],
            &files,
        );

        let watch_ref = cade.persist_watch_state("bigsession", &state).unwrap();

        assert!(watch_ref.len() < 512);
        assert_eq!(load_watch_ref(&watch_ref).unwrap().files.len(), 5000);
        std::fs::remove_dir_all(state_dir).ok();
    }

    #[test]
    fn load_watch_ref_reads_legacy_inline_json() {
        let raw = r#"{"version":"layer-cache-v3","root":"/project","cade_paths":["/project"],"files":[]}"#;
        assert_eq!(load_watch_ref(raw).unwrap().root_string(), "/project");
    }

    #[test]
    fn watch_state_round_trips_through_json() {
        let state = WatchState {
            version: LAYER_CACHE_VERSION.to_string(),
            root: PathBuf::from("/project"),
            cade_paths: vec![PathBuf::from("/project")],
            files: vec![WatchEntry {
                path: PathBuf::from("/project/.envrc"),
                state: WatchFileState::Present {
                    mtime: 1_780_000_000_000_000_000,
                    size: 10,
                    content_hash: Some(42),
                },
            }],
        };

        let raw = serde_json::to_string(&state).unwrap();
        let restored: WatchState = serde_json::from_str(&raw).unwrap();

        assert_eq!(restored.root, PathBuf::from("/project"));
        assert_eq!(
            restored.cade_path_set(),
            BTreeSet::from([String::from("/project")])
        );
    }
}
