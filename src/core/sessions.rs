mod gc_roots;
mod identity;
mod leases;

use crate::config;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub(super) use identity::{
    atomic_write, direnv_fallback_session_id, direnv_session_id, is_valid_session, new_session_id,
};

const DEFAULT_SHELL_GC_ROOT_TTL_SECS: u64 = 30 * 24 * 3600;

fn shell_gc_root_ttl() -> Duration {
    Duration::from_secs(
        config::shell_gc_root_ttl_seconds().unwrap_or(DEFAULT_SHELL_GC_ROOT_TTL_SECS),
    )
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum SessionHolder {
    Process {
        pid: u32,
        start_time: String,
        last_seen: u64,
    },
    Lease {
        client_id: String,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(super) struct LeaseRecord {
    pub(super) client_id: String,
    pub(super) kind: String,
    pub(super) project: Option<String>,
    pub(super) expires_at: u64,
    pub(super) last_seen: u64,
}

impl LeaseRecord {
    pub(super) fn session_holder(&self) -> SessionHolder {
        SessionHolder::Lease {
            client_id: self.client_id.clone(),
        }
    }
}
