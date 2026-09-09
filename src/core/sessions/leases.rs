use crate::core::Cade;
use crate::core::sessions::LeaseRecord;
use crate::core::sessions::SessionHolder;
use crate::core::sessions::gc_roots::{remove_session_holder, shell_gc_roots_dir};
use crate::core::sessions::identity::{atomic_write, new_client_id, now_secs, validate_client_id};
use crate::core::sessions::shell_gc_root_ttl;
use anyhow::{Context as _, Result, bail};
use serde::Serialize;
use std::{
    fs::{create_dir_all, read_dir, read_to_string, remove_file},
    path::{Path, PathBuf},
    time::Duration,
};

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

fn lease_dir(cade: &Cade) -> PathBuf {
    cade.state_dir.join("leases")
}

fn lease_path(cade: &Cade, client_id: &str) -> PathBuf {
    lease_dir(cade).join(format!("{client_id}.json"))
}

pub(super) fn read_lease_record(cade: &Cade, client_id: &str) -> Result<LeaseRecord> {
    validate_client_id(client_id)?;
    let raw = read_to_string(lease_path(cade, client_id))
        .with_context(|| format!("reading cade lease {client_id}"))?;
    let lease: LeaseRecord = serde_json::from_str(&raw).context("parse cade lease")?;
    if lease.client_id != client_id {
        bail!("cade lease {client_id} has mismatched client id");
    }
    Ok(lease)
}

fn write_lease_record(cade: &Cade, lease: &LeaseRecord) -> Result<()> {
    validate_client_id(&lease.client_id)?;
    create_dir_all(lease_dir(cade)).context("create cade leases dir")?;
    let body = serde_json::to_vec(lease).context("serialise cade lease")?;
    atomic_write(&lease_path(cade, &lease.client_id), &body).context("write cade lease")
}

fn refresh_lease_record(
    cade: &Cade,
    client_id: &str,
    ttl_seconds: Option<u64>,
) -> Result<LeaseRecord> {
    let existing = read_lease_record(cade, client_id)?;
    let ttl = ttl_seconds.map_or_else(shell_gc_root_ttl, Duration::from_secs);
    let lease = LeaseRecord {
        client_id: existing.client_id,
        kind: existing.kind,
        project: existing.project,
        expires_at: now_secs().saturating_add(ttl.as_secs()),
        last_seen: now_secs(),
    };
    write_lease_record(cade, &lease)?;
    Ok(lease)
}

pub fn lease_open(
    cade: &Cade,
    kind: &str,
    project: Option<&Path>,
    ttl_seconds: Option<u64>,
) -> Result<()> {
    if kind.is_empty()
        || !kind
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("lease kind must contain only letters, digits, '-' or '_'")
    }
    let ttl = ttl_seconds.map_or_else(shell_gc_root_ttl, Duration::from_secs);
    let lease = LeaseRecord {
        client_id: new_client_id(),
        kind: kind.to_owned(),
        project: project.map(|project_path| project_path.to_string_lossy().to_string()),
        expires_at: now_secs().saturating_add(ttl.as_secs()),
        last_seen: now_secs(),
    };
    write_lease_record(cade, &lease)?;
    let response = LeaseResponse::from(&lease);
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

pub fn lease_refresh(cade: &Cade, client_id: &str, ttl_seconds: Option<u64>) -> Result<()> {
    let lease = refresh_lease_record(cade, client_id, ttl_seconds)?;
    let response = LeaseResponse::from(&lease);
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

pub fn lease_close(cade: &Cade, client_id: &str) -> Result<()> {
    validate_client_id(client_id)?;
    let _ = remove_file(lease_path(cade, client_id));
    if let Ok(entries) = read_dir(shell_gc_roots_dir(cade)) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(session) = path.file_name().and_then(|file_name| file_name.to_str()) else {
                continue;
            };
            remove_session_holder(cade, session, &SessionHolder::lease(client_id.to_owned()));
        }
    }
    Ok(())
}
