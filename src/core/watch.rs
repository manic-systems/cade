use std::{
    collections::BTreeSet,
    fs::{
        Metadata,
        OpenOptions,
        create_dir_all,
        metadata,
        read_to_string,
    },
    io::Read as _,
    os::unix::fs::OpenOptionsExt as _,
    path::{
        Path,
        PathBuf,
    },
    time::UNIX_EPOCH,
};

use anyhow::{
    Context as _,
    Result,
    bail,
};
use serde::{
    Deserialize,
    Serialize,
};

use crate::{
    core::{
        Cade,
        cache::{
            get_watch_discovery,
            store_watch_discovery,
        },
        layer::tokenize_args,
        sessions::{
            atomic_write,
            is_valid_session,
            stable_hash_hex,
        },
    },
    types::keyword::Keyword,
};

pub(super) const LAYER_CACHE_VERSION: &str = "layer-cache-v6";

#[derive(Debug, PartialEq, Eq)]
pub(super) enum WatchChange {
    Unchanged,
    Metadata,
    Content,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct WatchEntry {
    path:  PathBuf,
    state: WatchFileState,
}

impl WatchEntry {
    fn capture(path: &Path) -> Self {
        Self {
            path:  path.to_path_buf(),
            state: watch_file_state(path),
        }
    }

    fn refresh(&mut self) -> WatchChange {
        self.state.refresh(&self.path)
    }

    fn token_part(&self) -> String {
        match self.state {
            WatchFileState::Present {
                mtime,
                size,
                content_hash,
            } => {
                content_hash.map_or_else(
                    || format!("{}:present-unreadable:{mtime}:{size}", self.path.display()),
                    |hash| format!("{}:present:{size}:{hash:016x}", self.path.display()),
                )
            },
            WatchFileState::Missing => format!("{}:missing", self.path.display()),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum WatchFileState {
    Present {
        mtime:        u64,
        size:         u64,
        content_hash: Option<u64>,
    },
    Missing,
}

impl WatchFileState {
    fn refresh(&mut self, path: &Path) -> WatchChange {
        let Ok(meta) = metadata(path) else {
            return if *self == Self::Missing {
                WatchChange::Unchanged
            } else {
                WatchChange::Content
            };
        };
        let current_mtime = mtime_nanos(&meta);
        let current_size = meta.len();

        match *self {
            Self::Missing => WatchChange::Content,
            Self::Present {
                ref mut mtime,
                ref mut size,
                ref mut content_hash,
            } => {
                if *mtime == current_mtime && *size == current_size {
                    return WatchChange::Unchanged;
                }
                if *size != current_size {
                    return WatchChange::Content;
                }
                if content_hash.is_none() || content_hash_for(path, &meta) != *content_hash {
                    return WatchChange::Content;
                }

                *mtime = current_mtime;
                WatchChange::Metadata
            },
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct WatchState {
    #[serde(default)]
    version:    String,
    root:       PathBuf,
    cade_paths: Vec<PathBuf>,
    files:      Vec<WatchEntry>,
}

impl WatchState {
    pub(super) fn capture(
        root: &Path,
        cade_paths: Vec<PathBuf>,
        watched_files: &[PathBuf],
    ) -> Self {
        Self {
            version: LAYER_CACHE_VERSION.to_owned(),
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

    pub(super) fn refresh(&mut self) -> WatchChange {
        if self.version != LAYER_CACHE_VERSION {
            return WatchChange::Content;
        }

        let mut change = WatchChange::Unchanged;
        for entry in &mut self.files {
            match entry.refresh() {
                WatchChange::Unchanged => {},
                WatchChange::Metadata => change = WatchChange::Metadata,
                WatchChange::Content => return WatchChange::Content,
            }
        }
        change
    }
}

// Named by hash rather than session so a subshell's reload doesn't
// replace the file its parent still diffs against.
pub(super) fn persist_watch_state(
    cade: &Cade,
    session: &str,
    watches: &WatchState,
) -> Result<String> {
    if !is_valid_session(session) {
        bail!("invalid cade session id")
    }
    let body = serde_json::to_vec(watches).context("serialize watch state")?;
    let dir = cade.state_dir.join("watches");
    create_dir_all(&dir).context("create watches dir")?;
    let hash = stable_hash_hex(&String::from_utf8_lossy(&body));
    let path = dir.join(format!("{session}-{hash}.json"));
    atomic_write(&path, &body).context("write watch state")?;
    Ok(path.to_string_lossy().to_string())
}

// Inline json is the pre-file format still living in older shells.
pub fn load_watch_ref(raw: &str) -> Option<WatchState> {
    if raw.starts_with('{') {
        return serde_json::from_str(raw).ok();
    }
    let body = read_to_string(raw).ok()?;
    serde_json::from_str(&body).ok()
}

// The walk is skipped while every file found last time keeps its mtime and
// size, so a new file goes unseen until an already-watched one changes.
pub(super) fn layer_watch(
    cade: &Cade,
    dir: &Path,
    keywords: &[Keyword],
) -> Result<(Vec<PathBuf>, String)> {
    let key = dir.to_string_lossy();
    if let Some((files, token)) = get_watch_discovery(cade, &key)
        && compute_layer_key(&files) == token
    {
        return Ok((files, token));
    }

    let files = watched_files_for_keywords(dir, keywords)?;
    let token = compute_layer_key(&files);
    store_watch_discovery(cade, &key, &files, &token)?;
    Ok((files, token))
}

fn watched_files_for_keywords(dir: &Path, keywords: &[Keyword]) -> Result<Vec<PathBuf>> {
    let mut files = vec![dir.join(".cade")];
    for kw in keywords {
        match *kw {
            Keyword::Load(ref loadable) => files.extend(loadable.resolve(dir).watch),
            Keyword::Watch(ref raw) => {
                files.extend(tokenize_args(raw)?.iter().map(|arg| dir.join(arg)));
            },
            Keyword::Pure
            | Keyword::Disinherit
            | Keyword::Call(_)
            | Keyword::Hook(_)
            | Keyword::Clear(_)
            | Keyword::Concat(_)
            | Keyword::Set(_) => {},
        }
    }
    Ok(files)
}

pub(super) fn compute_layer_key(watched_files: &[PathBuf]) -> String {
    let mut parts = vec![LAYER_CACHE_VERSION.to_owned()];
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
    metadata(path).map_or(WatchFileState::Missing, |meta| {
        WatchFileState::Present {
            mtime:        mtime_nanos(&meta),
            size:         meta.len(),
            content_hash: content_hash_for(path, &meta),
        }
    })
}

fn content_hash_for(path: &Path, meta: &Metadata) -> Option<u64> {
    if !meta.is_file() {
        return None;
    }

    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }

    let mut hash = 0xCBF2_9CE4_8422_2325_u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            return Some(hash);
        }
        for byte in &buffer[..read] {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01B3);
        }
    }
}

fn mtime_nanos(meta: &Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_nanos().min(u128::from(u64::MAX))).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        env::temp_dir,
        fs::{
            FileTimes,
            OpenOptions,
            create_dir_all,
            metadata,
            remove_dir_all,
            write,
        },
        path::{
            Path,
            PathBuf,
        },
        process::id,
        slice::from_ref,
        thread::current,
        time::Duration,
    };

    use crate::{
        core::{
            Cade,
            watch::{
                LAYER_CACHE_VERSION,
                WatchChange,
                WatchEntry,
                WatchFileState,
                WatchState,
                compute_layer_key,
                layer_watch,
                load_watch_ref,
                persist_watch_state,
            },
        },
        types::keyword::{
            Keyword,
            Loadable,
        },
    };

    #[test]
    fn watch_discovery_refreshes_only_when_a_watched_file_changes() {
        let root = temp_dir().join(format!(
            "cade-watch-discovery-{}-{}",
            id(),
            current().name().unwrap_or("test")
        ));
        create_dir_all(root.join("nix")).unwrap();
        write(root.join(".envrc"), "use flake\n").unwrap();
        write(root.join("flake.nix"), "{}\n").unwrap();

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
        let keywords = [Keyword::Load(Loadable::Envrc(String::new()))];
        let extra = root.join("nix").join("extra.nix");

        let (first, _) = layer_watch(&cade, &root, &keywords).unwrap();
        assert!(first.contains(&root.join("flake.nix")));
        assert!(!first.contains(&extra));

        write(&extra, "{}\n").unwrap();
        let (reused, _) = layer_watch(&cade, &root, &keywords).unwrap();
        assert!(!reused.contains(&extra));

        write(root.join("flake.nix"), "{ inputs = {}; }\n").unwrap();
        let (rediscovered, _) = layer_watch(&cade, &root, &keywords).unwrap();
        assert!(rediscovered.contains(&extra));

        let _ = remove_dir_all(&root);
    }

    #[test]
    fn timestamp_only_changes_do_not_invalidate_content_identity() {
        let root = temp_dir().join(format!(
            "cade-watch-content-{}-{}",
            id(),
            current().name().unwrap_or("test")
        ));
        create_dir_all(&root).unwrap();
        let path = root.join("flake.nix");
        write(&path, "same\n").unwrap();

        let mut entry = WatchEntry::capture(&path);
        let token = compute_layer_key(from_ref(&path));
        let old_mtime = metadata(&path).unwrap().modified().unwrap();
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_times(FileTimes::new().set_modified(old_mtime + Duration::from_secs(1)))
            .unwrap();

        assert_eq!(entry.refresh(), WatchChange::Metadata);
        assert_eq!(compute_layer_key(from_ref(&path)), token);

        write(&path, "else\n").unwrap();
        assert_eq!(entry.refresh(), WatchChange::Content);
        assert_ne!(compute_layer_key(from_ref(&path)), token);
        let _ = remove_dir_all(root);
    }

    #[test]
    fn old_watch_state_versions_are_stale() {
        let mut state = WatchState {
            version:    "layer-cache-v2".to_owned(),
            root:       PathBuf::from("/project"),
            cade_paths: vec![PathBuf::from("/project")],
            files:      Vec::new(),
        };

        assert_eq!(state.refresh(), WatchChange::Content);
    }

    #[test]
    fn missing_watch_state_version_is_stale() {
        let raw = r#"{"root":"/project","cade_paths":["/project"],"files":[]}"#;
        let mut state: WatchState = serde_json::from_str(raw).unwrap();

        assert_eq!(state.refresh(), WatchChange::Content);
    }

    #[test]
    fn watch_ref_stays_short_for_huge_watch_lists() {
        let state_dir = temp_dir().join(format!("cade-watchref-{}", id()));
        create_dir_all(&state_dir).unwrap();
        let cade = Cade {
            db:        rusqlite::Connection::open_in_memory().unwrap(),
            cwd:       state_dir.clone(),
            state_dir: state_dir.clone(),
        };
        let files = (0_i32..5_000_i32)
            .map(|index| {
                PathBuf::from(format!(
                    "/project/third_party/component-{index}/package.json"
                ))
            })
            .collect::<Vec<PathBuf>>();
        let state = WatchState::capture(
            Path::new("/project"),
            vec![PathBuf::from("/project")],
            &files,
        );

        let watch_ref = persist_watch_state(&cade, "bigsession", &state).unwrap();

        assert!(watch_ref.len() < 512);
        assert_eq!(load_watch_ref(&watch_ref).unwrap().files.len(), 5000);
        let _ = remove_dir_all(state_dir);
    }

    #[test]
    fn load_watch_ref_reads_legacy_inline_json() {
        let raw = r#"{"version":"layer-cache-v3","root":"/project","cade_paths":["/project"],"files":[]}"#;
        assert_eq!(load_watch_ref(raw).unwrap().root_string(), "/project");
    }

    #[test]
    fn watch_state_round_trips_through_json() {
        let state = WatchState {
            version:    LAYER_CACHE_VERSION.to_owned(),
            root:       PathBuf::from("/project"),
            cade_paths: vec![PathBuf::from("/project")],
            files:      vec![WatchEntry {
                path:  PathBuf::from("/project/.envrc"),
                state: WatchFileState::Present {
                    mtime:        1_780_000_000_000_000_000,
                    size:         10,
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
