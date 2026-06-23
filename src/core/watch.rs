use super::layer::tokenize_args;
use crate::types::Keyword;
use anyhow::Result;
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
