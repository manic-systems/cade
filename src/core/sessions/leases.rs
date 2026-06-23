use super::{
    LeaseRecord, SessionHolder,
    identity::{atomic_write, new_client_id, now_secs, validate_client_id},
    shell_gc_root_ttl,
};
use crate::core::Cade;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::{path::Path, time::Duration};

#[derive(Debug, Serialize)]
struct LeaseResponse {
    client_id: String,
    kind: String,
    project: Option<String>,
    expires_at: u64,
}

impl From<&LeaseRecord> for LeaseResponse {
    fn from(lease: &LeaseRecord) -> Self {
        Self {
            client_id: lease.client_id.clone(),
            kind: lease.kind.clone(),
            project: lease.project.clone(),
            expires_at: lease.expires_at,
        }
    }
}

pub(super) fn lease_record_is_live(lease: &LeaseRecord) -> bool {
    lease.expires_at > now_secs()
}

impl Cade {
    fn lease_dir(&self) -> std::path::PathBuf {
        self.state_dir.join("leases")
    }

    fn lease_path(&self, client_id: &str) -> std::path::PathBuf {
        self.lease_dir().join(format!("{client_id}.json"))
    }

    pub(super) fn read_lease_record(&self, client_id: &str) -> Result<LeaseRecord> {
        validate_client_id(client_id)?;
        let raw = std::fs::read_to_string(self.lease_path(client_id))
            .with_context(|| format!("reading cade lease {client_id}"))?;
        let lease: LeaseRecord = serde_json::from_str(&raw).context("parse cade lease")?;
        if lease.client_id != client_id {
            bail!("cade lease {client_id} has mismatched client id");
        }
        Ok(lease)
    }

    fn write_lease_record(&self, lease: &LeaseRecord) -> Result<()> {
        validate_client_id(&lease.client_id)?;
        std::fs::create_dir_all(self.lease_dir()).context("create cade leases dir")?;
        let body = serde_json::to_vec(lease).context("serialise cade lease")?;
        atomic_write(&self.lease_path(&lease.client_id), &body).context("write cade lease")
    }

    fn refresh_lease_record(
        &self,
        client_id: &str,
        ttl_seconds: Option<u64>,
    ) -> Result<LeaseRecord> {
        let existing = self.read_lease_record(client_id)?;
        let ttl = ttl_seconds
            .map(Duration::from_secs)
            .unwrap_or_else(shell_gc_root_ttl);
        let lease = LeaseRecord {
            client_id: existing.client_id,
            kind: existing.kind,
            project: existing.project,
            expires_at: now_secs().saturating_add(ttl.as_secs()),
            last_seen: now_secs(),
        };
        self.write_lease_record(&lease)?;
        Ok(lease)
    }

    pub fn lease_open(
        &self,
        kind: &str,
        project: Option<&Path>,
        ttl_seconds: Option<u64>,
    ) -> Result<()> {
        if kind.is_empty()
            || !kind
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            bail!("lease kind must contain only letters, digits, '-' or '_'")
        }
        let ttl = ttl_seconds
            .map(Duration::from_secs)
            .unwrap_or_else(shell_gc_root_ttl);
        let lease = LeaseRecord {
            client_id: new_client_id(),
            kind: kind.to_string(),
            project: project.map(|p| p.to_string_lossy().to_string()),
            expires_at: now_secs().saturating_add(ttl.as_secs()),
            last_seen: now_secs(),
        };
        self.write_lease_record(&lease)?;
        let response = LeaseResponse::from(&lease);
        println!("{}", serde_json::to_string(&response)?);
        Ok(())
    }

    pub fn lease_refresh(&self, client_id: &str, ttl_seconds: Option<u64>) -> Result<()> {
        let lease = self.refresh_lease_record(client_id, ttl_seconds)?;
        let response = LeaseResponse::from(&lease);
        println!("{}", serde_json::to_string(&response)?);
        Ok(())
    }

    pub fn lease_close(&self, client_id: &str) -> Result<()> {
        validate_client_id(client_id)?;
        std::fs::remove_file(self.lease_path(client_id)).ok();
        if let Ok(entries) = std::fs::read_dir(self.shell_gc_roots_dir()) {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(session) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                self.remove_session_holder(session, &SessionHolder::lease(client_id.to_string()));
            }
        }
        Ok(())
    }
}
