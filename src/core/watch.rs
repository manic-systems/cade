use super::layer::tokenize_args;
use crate::types::Keyword;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const LAYER_CACHE_VERSION: &str = "layer-cache-v3";

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct WatchEntry {
    path: String,
    state: WatchFileState,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum WatchFileState {
    Present { mtime: u128, size: u64 },
    Missing,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct WatchState {
    pub(super) root: String,
    pub(super) cade_paths: Vec<String>,
    files: Vec<WatchEntry>,
}

pub(super) fn watched_files_for_keywords(dir: &Path, keywords: &[Keyword]) -> Result<Vec<PathBuf>> {
    let mut files = vec![dir.join(".cade")];
    for kw in keywords {
        match kw {
            // share load resolution so missing targets are watched lexically
            Keyword::Load(loadable) => files.extend(loadable.resolve(dir).watch),
            Keyword::Watch(raw) => files.extend(tokenize_args(raw)?.iter().map(|w| dir.join(w))),
            _ => {}
        }
    }
    Ok(files)
}

pub(super) fn compute_layer_key(watched_files: &[PathBuf]) -> String {
    let mut parts = vec![LAYER_CACHE_VERSION.to_string()];
    for file in watched_files {
        match std::fs::metadata(file) {
            Ok(meta) => parts.push(format!(
                "{}:present:{}:{}",
                file.display(),
                mtime_nanos(&meta),
                meta.len()
            )),
            Err(_) => parts.push(format!("{}:missing", file.display())),
        }
    }
    parts.join("\n")
}

pub(super) fn build_watch_state(
    root: &Path,
    cade_paths: Vec<String>,
    watched_files: &[PathBuf],
) -> WatchState {
    let files = watched_files
        .iter()
        .map(|f| WatchEntry {
            path: f.to_string_lossy().to_string(),
            state: watch_file_state(f),
        })
        .collect();

    WatchState {
        root: root.to_string_lossy().to_string(),
        cade_paths,
        files,
    }
}

pub(super) fn files_changed(state: &WatchState) -> bool {
    state
        .files
        .iter()
        .any(|entry| watch_file_state(Path::new(&entry.path)) != entry.state)
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

fn mtime_nanos(meta: &std::fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
