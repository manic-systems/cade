pub mod gc_roots;
pub mod identity;
pub mod leases;

use std::time::Duration;

use anyhow::{
    Result,
    bail,
};
use identity::validate_client_id;
pub(super) use identity::{
    atomic_write,
    direnv_fallback_session_id,
    direnv_session_id,
    is_valid_session,
    new_session_id,
    stable_hash_hex,
};
use serde::{
    Deserialize,
    Serialize,
};

use crate::config;

const DEFAULT_SHELL_GC_ROOT_TTL_SECS: u64 = 30 * 24 * 3600;

pub(super) fn shell_gc_root_ttl() -> Duration {
    Duration::from_secs(
        config::shell_gc_root_ttl_seconds().unwrap_or(DEFAULT_SHELL_GC_ROOT_TTL_SECS),
    )
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum SessionHolder {
    Process {
        pid:        u32,
        start_time: String,
        last_seen:  u64,
    },
    Lease {
        client_id: String,
    },
}

impl SessionHolder {
    pub(super) const fn process(pid: u32, start_time: String, last_seen: u64) -> Self {
        Self::Process {
            pid,
            start_time,
            last_seen,
        }
    }

    pub(super) const fn lease(client_id: String) -> Self {
        Self::Lease { client_id }
    }

    pub(super) fn file_name(&self) -> Result<String> {
        match *self {
            Self::Process {
                pid,
                ref start_time,
                ..
            } => {
                if start_time
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                {
                    Ok(format!("process-{pid}-{start_time}.json"))
                } else {
                    bail!("invalid process start time")
                }
            },
            Self::Lease { ref client_id } => {
                validate_client_id(client_id)?;
                Ok(format!("lease-{client_id}.json"))
            },
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(super) struct LeaseRecord {
    pub client_id:  String,
    pub kind:       String,
    pub project:    Option<String>,
    pub expires_at: u64,
    pub last_seen:  u64,
}

impl LeaseRecord {
    pub(super) fn session_holder(&self) -> SessionHolder {
        SessionHolder::lease(self.client_id.clone())
    }
}
