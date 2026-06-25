use crate::env::{EnvDelta, is_shell_managed, live_ambient_env};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

const DIRENV_DIFF: &str = "DIRENV_DIFF";
const DIRENV_DIR: &str = "DIRENV_DIR";
const DIRENV_FILE: &str = "DIRENV_FILE";
const DIRENV_WATCHES: &str = "DIRENV_WATCHES";

pub struct ExportMetadata {
    pub root: String,
    pub file: String,
    pub watches: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExportState {
    version: u8,
    preimage: HashMap<String, Option<String>>,
}

pub struct ExportSession {
    pub live: HashMap<String, String>,
    pub baseline: HashMap<String, String>,
    pub previous: Option<ExportState>,
}

pub fn capture_session(snapshot: Option<HashMap<String, String>>) -> ExportSession {
    let live = live_ambient_env();
    let previous = export_state(&live);

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

pub fn inactive_delta(previous: Option<ExportState>) -> EnvDelta {
    let Some(previous) = previous else {
        return EnvDelta::empty();
    };

    let mut delta = EnvDelta::empty();
    restore_applied(&mut delta, &previous);
    delta.record(DIRENV_DIFF, None);
    delta.record(DIRENV_DIR, None);
    delta.record(DIRENV_FILE, None);
    delta.record(DIRENV_WATCHES, None);
    delta
}

pub fn active_delta(
    mut delta: EnvDelta,
    baseline: HashMap<String, String>,
    previous: Option<ExportState>,
    metadata: ExportMetadata,
) -> Result<EnvDelta> {
    let applied = tracked_delta_keys(&delta);

    if let Some(previous) = previous.as_ref() {
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
    delta.record(DIRENV_DIR, Some(format!("-{}", metadata.root)));
    delta.record(DIRENV_FILE, Some(metadata.file));
    delta.record(
        DIRENV_WATCHES,
        Some(serde_json::to_string(&metadata.watches)?),
    );
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
    baseline.remove(DIRENV_DIR);
    baseline.remove(DIRENV_FILE);
    baseline.remove(DIRENV_WATCHES);
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
    !matches!(key, DIRENV_DIFF | DIRENV_DIR | DIRENV_FILE | DIRENV_WATCHES)
        && !is_shell_managed(key)
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
