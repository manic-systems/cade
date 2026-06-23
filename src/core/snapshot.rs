use super::{
    Cade,
    sessions::{atomic_write, is_valid_session},
};
use anyhow::{Context, Result};
use std::{collections::HashMap, path::PathBuf};

impl Cade {
    pub(super) fn snapshot_path(&self, session: &str) -> PathBuf {
        self.state_dir
            .join("snapshots")
            .join(format!("{session}.env"))
    }

    pub(super) fn read_snapshot(&self, session: &str) -> Option<HashMap<String, String>> {
        if !is_valid_session(session) {
            return None;
        }
        let raw = std::fs::read_to_string(self.snapshot_path(session)).ok()?;
        serde_json::from_str(&raw)
            .ok()
            .or_else(|| Some(read_legacy_snapshot(&raw)))
    }

    pub(super) fn write_snapshot(
        &self,
        session: &str,
        env: &HashMap<String, String>,
    ) -> Result<()> {
        let dir = self.state_dir.join("snapshots");
        std::fs::create_dir_all(&dir).context("create snapshots dir")?;
        let body = serde_json::to_vec(env).context("serialize snapshot")?;
        atomic_write(&self.snapshot_path(session), &body).context("write snapshot")
    }
}

fn read_legacy_snapshot(raw: &str) -> HashMap<String, String> {
    raw.split('\x1F')
        .filter_map(|entry| {
            entry
                .split_once('=')
                .map(|(key, value)| (key.to_string(), value.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cade_for_state_dir(state_dir: PathBuf) -> Cade {
        Cade {
            db: rusqlite::Connection::open_in_memory().unwrap(),
            cwd: state_dir.clone(),
            state_dir,
        }
    }

    fn temp_state_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cade-snapshot-{name}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn snapshot_round_trips_values_with_legacy_separators_and_equals() {
        let state_dir = temp_state_dir("json-roundtrip");
        let cade = cade_for_state_dir(state_dir.clone());
        let env = HashMap::from([
            ("A".to_string(), "one\x1ftwo".to_string()),
            ("B".to_string(), "x=y".to_string()),
        ]);

        cade.write_snapshot("session", &env).unwrap();

        assert_eq!(cade.read_snapshot("session").unwrap(), env);
        std::fs::remove_dir_all(state_dir).ok();
    }

    #[test]
    fn legacy_snapshot_format_still_reads() {
        assert_eq!(
            read_legacy_snapshot("A=one\x1fB=x=y"),
            HashMap::from([
                ("A".to_string(), "one".to_string()),
                ("B".to_string(), "x=y".to_string()),
            ])
        );
    }
}
