use crate::core::Cade;
use crate::core::sessions::{atomic_write, is_valid_session};
use anyhow::{Context as _, Result};
use std::{
    collections::BTreeMap,
    fs::{create_dir_all, read_to_string},
    path::PathBuf,
};

pub(super) fn snapshot_path(cade: &Cade, session: &str) -> PathBuf {
    cade.state_dir
        .join("snapshots")
        .join(format!("{session}.env"))
}

pub(super) fn read_snapshot(cade: &Cade, session: &str) -> Option<BTreeMap<String, String>> {
    if !is_valid_session(session) {
        return None;
    }
    let raw = read_to_string(snapshot_path(cade, session)).ok()?;
    serde_json::from_str(&raw)
        .ok()
        .or_else(|| Some(read_legacy_snapshot(&raw)))
}

pub(super) fn write_snapshot(
    cade: &Cade,
    session: &str,
    env: &BTreeMap<String, String>,
) -> Result<()> {
    let dir = cade.state_dir.join("snapshots");
    create_dir_all(&dir).context("create snapshots dir")?;
    let body = serde_json::to_vec(env).context("serialize snapshot")?;
    atomic_write(&snapshot_path(cade, session), &body).context("write snapshot")
}

fn read_legacy_snapshot(raw: &str) -> BTreeMap<String, String> {
    raw.split('\x1F')
        .filter_map(|entry| {
            entry
                .split_once('=')
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::core::Cade;
    use crate::core::snapshot::{read_legacy_snapshot, read_snapshot, write_snapshot};
    use std::collections::BTreeMap;
    use std::env::temp_dir;
    use std::fs::{create_dir_all, remove_dir_all};
    use std::path::PathBuf;
    use std::process::id;
    use std::thread::current;

    fn cade_for_state_dir(state_dir: PathBuf) -> Cade {
        Cade {
            db: rusqlite::Connection::open_in_memory().unwrap(),
            cwd: state_dir.clone(),
            state_dir,
        }
    }

    fn temp_state_dir(name: &str) -> PathBuf {
        let dir = temp_dir().join(format!(
            "cade-snapshot-{name}-{}-{}",
            id(),
            current().name().unwrap_or("test")
        ));
        let _ = remove_dir_all(&dir);
        create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn snapshot_round_trips_values_with_legacy_separators_and_equals() {
        let state_dir = temp_state_dir("json-roundtrip");
        let cade = cade_for_state_dir(state_dir.clone());
        let env = BTreeMap::from([
            ("A".to_owned(), "one\x1ftwo".to_owned()),
            ("B".to_owned(), "x=y".to_owned()),
        ]);

        write_snapshot(&cade, "session", &env).unwrap();

        assert_eq!(read_snapshot(&cade, "session").unwrap(), env);
        let _ = remove_dir_all(state_dir);
    }

    #[test]
    fn legacy_snapshot_format_still_reads() {
        assert_eq!(
            read_legacy_snapshot("A=one\x1fB=x=y"),
            BTreeMap::from([
                ("A".to_owned(), "one".to_owned()),
                ("B".to_owned(), "x=y".to_owned()),
            ])
        );
    }
}
