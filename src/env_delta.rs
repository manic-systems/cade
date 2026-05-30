//! Shared environment-diff model.
//!
//! Shell activation and `export json` must compute the same variable changes.
//! Keeping the diff here avoids maintaining a second activation implementation
//! for editor/direnv compatibility.

use crate::shells::{self, ShellOutput};
use std::collections::{BTreeMap, HashMap, HashSet};

type EnvDiff = BTreeMap<String, Option<String>>;

pub(crate) struct EnvDelta {
    changes: EnvDiff,
}

pub(crate) struct EnvDeltaInput<'a> {
    pub(crate) env: &'a HashMap<String, Vec<String>>,
    pub(crate) absorb: &'a HashSet<String>,
    pub(crate) unset: &'a [String],
    pub(crate) purified: bool,
    /// Current process environment after any previous cade/direnv activation.
    pub(crate) live_env: &'a HashMap<String, String>,
    /// Pre-activation environment for concat/restore semantics.
    pub(crate) baseline: &'a HashMap<String, String>,
}

impl EnvDelta {
    pub(crate) fn empty() -> Self {
        Self {
            changes: EnvDiff::new(),
        }
    }

    pub(crate) fn from_rollup(input: EnvDeltaInput<'_>) -> Self {
        let EnvDeltaInput {
            env,
            absorb,
            unset,
            purified,
            live_env,
            baseline,
        } = input;
        let mut changes = EnvDiff::new();

        if purified {
            // Pure activation must clear variables from both the current
            // environment and the original baseline; otherwise a later restore
            // path can leave behind variables that were absent from `live_env`.
            for k in live_env.keys().chain(baseline.keys()) {
                if !is_pure_preserved_key(k) {
                    record_change(&mut changes, k, None);
                }
            }
        }

        for k in unset {
            record_change(&mut changes, k, None);
        }

        for (k, v) in env {
            let mut value = v.join(":");
            // concat vars keep ambient values, appended after .cade values
            if !purified
                && absorb.contains(k)
                && let Some(amb) = baseline.get(k).filter(|a| !a.is_empty())
            {
                value = format!("{value}:{amb}");
            }
            record_change(&mut changes, k, Some(value));
        }

        Self { changes }
    }

    pub(crate) fn render_shell(&self, shell: &dyn ShellOutput) -> String {
        let mut output = String::new();
        for (k, v) in &self.changes {
            match v {
                Some(value) => output.push_str(&shell.set_env(k, value)),
                None => output.push_str(&shell.unset_env(k)),
            }
        }
        output
    }

    pub(crate) fn contains(&self, key: &str) -> bool {
        self.changes.contains_key(key)
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = &str> {
        self.changes.keys().map(String::as_str)
    }

    pub(crate) fn record(&mut self, key: &str, value: Option<String>) {
        record_change(&mut self.changes, key, value);
    }

    pub(crate) fn to_json(&self) -> String {
        format!(
            "{}\n",
            serde_json::to_string(&self.changes).expect("env diff serializes")
        )
    }
}

pub(crate) fn live_ambient_env() -> HashMap<String, String> {
    // Cade internals describe the active session, not the user's ambient
    // environment. Treating them as ambient would leak activation bookkeeping
    // through `export json` and into concat baselines.
    std::env::vars()
        .filter(|(k, _)| !k.starts_with("__CADE_"))
        .collect()
}

// Keys cade must never set from a layer: the shell owns them, or they're
// cade's own bookkeeping.
pub(crate) fn is_shell_managed(key: &str) -> bool {
    matches!(key, "PWD" | "OLDPWD" | "SHLVL" | "_" | "LAST_EXIT_CODE") || key.starts_with("__CADE_")
}

fn is_pure_preserved_key(key: &str) -> bool {
    is_shell_managed(key)
        || matches!(
            key,
            "HOME" | "CADE_VERBOSITY" | "CADE_LONG_RUNNING_WARNING_MS"
        )
}

fn record_change(changes: &mut EnvDiff, key: &str, value: Option<String>) {
    if shells::is_valid_key(key) {
        changes.insert(key.to_string(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_managed_classification() {
        for k in [
            "PWD",
            "OLDPWD",
            "SHLVL",
            "_",
            "LAST_EXIT_CODE",
            "__CADE_PREV",
            "__CADE_SET",
        ] {
            assert!(is_shell_managed(k), "{k} should be shell-managed");
        }
        for k in ["PATH", "HOME", "MY_VAR"] {
            assert!(!is_shell_managed(k), "{k} should not be shell-managed");
        }
        assert!(is_pure_preserved_key("HOME"));
    }

    #[test]
    fn json_escapes_separators() {
        let delta = EnvDelta {
            changes: EnvDiff::from([
                ("A".to_string(), Some("x\x1fy".to_string())),
                ("B".to_string(), None),
            ]),
        };
        let out = delta.to_json();
        // strict parse proves the U+001F separator was escaped, not emitted raw
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["A"], "x\x1fy");
        assert!(v["B"].is_null());
    }
}
