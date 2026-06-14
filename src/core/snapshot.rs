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

    pub(super) fn write_snapshot(
        &self,
        session: &str,
        env: &HashMap<String, String>,
    ) -> Result<()> {
        let dir = self.state_dir.join("snapshots");
        std::fs::create_dir_all(&dir).context("create snapshots dir")?;
        let body = env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("\x1F");
        atomic_write(&self.snapshot_path(session), body.as_bytes()).context("write snapshot")
    }
}
