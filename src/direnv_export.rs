//! Direnv-compatible JSON export for editors and tools such as Zed.
//!
//! `DIRENV_DIFF` only tracks the previous values for variables cade changed.
//! That keeps repeated `direnv export json` calls idempotent when the caller
//! preserves the returned project environment between calls.
//!
//! This is deliberately not cade's interactive shell path. Shell hooks get
//! `__CADE_SESSION` snapshots and hook bookkeeping; this adapter returns only
//! the environment diff expected by direnv-compatible tools.

use crate::env_delta::{EnvDelta, is_shell_managed, live_ambient_env};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

const DIRENV_DIFF: &str = "DIRENV_DIFF";

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ExportState {
    version: u8,
    preimage: HashMap<String, Option<String>>,
}

pub(crate) struct ExportSession {
    pub live: HashMap<String, String>,
    pub baseline: HashMap<String, String>,
    pub previous: Option<ExportState>,
}

pub(crate) fn capture_session(snapshot: Option<HashMap<String, String>>) -> ExportSession {
    let live = live_ambient_env();
    let previous = export_state(&live);
    // Prefer DIRENV_DIFF when present because it is the only state a direct
    // direnv caller, such as Zed, carries between invocations. Native cade
    // shells still use their __CADE_SESSION snapshot as the baseline.
    let baseline = previous
        .as_ref()
        .map(|state| state.baseline_from_live(&live))
        .or(snapshot)
        .unwrap_or_else(|| live_baseline(&live));

    ExportSession {
        live,
        baseline,
        previous,
    }
}

pub(crate) fn inactive_delta(previous: Option<ExportState>) -> EnvDelta {
    let Some(previous) = previous else {
        return EnvDelta::empty();
    };

    // Direnv asks for a diff on every directory change. When the new directory
    // has no active cade project, returning `{}` would leave the old project's
    // PATH and variables stuck in the editor environment.
    let mut delta = EnvDelta::empty();
    restore_applied(&mut delta, &previous);
    delta.record(DIRENV_DIFF, None);
    delta
}

pub(crate) fn active_delta(
    mut delta: EnvDelta,
    baseline: HashMap<String, String>,
    previous: Option<ExportState>,
) -> Result<EnvDelta> {
    let applied = tracked_delta_keys(&delta);

    if let Some(previous) = previous.as_ref() {
        // If a variable was changed by the previous activation but is not
        // produced by the new one, restore its preimage. This is what prevents
        // stale variables from surviving reloads or project switches.
        for (key, preimage) in previous.tracked_preimages() {
            if delta.contains(key) {
                continue;
            }
            delta.record(key, preimage.cloned());
        }
    }

    let preimage = applied
        .iter()
        .map(|key| {
            // Preserve the original preimage across repeated exports. Reading
            // from `live` here would make PATH grow as callers feed the prior
            // exported environment back into the next `direnv export json`.
            let value = previous
                .as_ref()
                .and_then(|state| state.preimage.get(key).cloned())
                .unwrap_or_else(|| baseline.get(key).cloned());
            (key.clone(), value)
        })
        .collect();
    let state = ExportState {
        version: 2,
        preimage,
    };
    delta.record(DIRENV_DIFF, Some(serde_json::to_string(&state)?));
    Ok(delta)
}

fn export_state(live: &HashMap<String, String>) -> Option<ExportState> {
    let state = live.get(DIRENV_DIFF)?;
    let state: ExportState = serde_json::from_str(state).ok()?;
    (state.version == 2).then_some(state)
}

fn live_baseline(live: &HashMap<String, String>) -> HashMap<String, String> {
    let mut baseline = live.clone();
    baseline.remove(DIRENV_DIFF);
    baseline
}

fn restore_applied(delta: &mut EnvDelta, previous: &ExportState) {
    for (key, preimage) in previous.tracked_preimages() {
        delta.record(key, preimage.cloned());
    }
}

fn tracked_delta_keys(delta: &EnvDelta) -> BTreeSet<String> {
    delta
        .keys()
        .filter(|key| is_tracked_key(key))
        .map(ToOwned::to_owned)
        .collect()
}

fn is_tracked_key(key: &str) -> bool {
    key != DIRENV_DIFF && !is_shell_managed(key)
}

impl ExportState {
    fn tracked_preimages(&self) -> impl Iterator<Item = (&str, Option<&String>)> {
        self.preimage
            .iter()
            .filter(|(key, _)| is_tracked_key(key))
            .map(|(key, value)| (key.as_str(), value.as_ref()))
    }

    fn baseline_from_live(&self, live: &HashMap<String, String>) -> HashMap<String, String> {
        let mut baseline = live_baseline(live);
        // Reconstruct the user's original environment by undoing only the
        // variables cade previously touched. Untouched variables are left as
        // they appear in the caller's current project environment.
        for (key, preimage) in &self.preimage {
            match preimage {
                Some(value) => {
                    baseline.insert(key.clone(), value.clone());
                }
                None => {
                    baseline.remove(key);
                }
            }
        }
        baseline
    }
}
