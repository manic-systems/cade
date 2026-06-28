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
    path::{Path, PathBuf},
};

pub(super) const LAYER_CACHE_VERSION: &str = "layer-cache-v3";

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
        watch_file_state(&self.path) != self.state
    }

    fn token_part(&self) -> String {
        match &self.state {
            WatchFileState::Present { mtime, size } => {
                format!("{}:present:{mtime}:{size}", self.path.display())
            }
            WatchFileState::Missing => format!("{}:missing", self.path.display()),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum WatchFileState {
    Present { mtime: u64, size: u64 },
    Missing,
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

pub(super) fn watched_files_for_keywords(dir: &Path, keywords: &[Keyword]) -> Result<Vec<PathBuf>> {
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
        },
        Err(_) => WatchFileState::Missing,
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
