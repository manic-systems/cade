use std::{
    collections::{
        BTreeMap,
        BTreeSet,
    },
    env::vars,
};

use crate::shells::{
    self,
    ShellOutput,
};

type EnvDiff = BTreeMap<String, Option<String>>;

pub struct EnvDelta {
    changes: EnvDiff,
}

#[derive(Clone, Copy)]
pub struct EnvDeltaInput<'input> {
    pub env:      &'input BTreeMap<String, Vec<String>>,
    pub absorb:   &'input BTreeSet<String>,
    pub unset:    &'input [String],
    pub purified: bool,
    pub live_env: &'input BTreeMap<String, String>,
    pub baseline: &'input BTreeMap<String, String>,
}

impl EnvDelta {
    pub const fn empty() -> Self {
        Self {
            changes: EnvDiff::new(),
        }
    }

    pub fn from_rollup(input: EnvDeltaInput<'_>) -> Self {
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
            for key in live_env.keys().chain(baseline.keys()) {
                if !is_pure_preserved_key(key) {
                    record_change(&mut changes, key, None);
                }
            }
        }

        for key in unset {
            record_change(&mut changes, key, None);
        }

        for (key, parts) in env {
            let mut joined = parts.join(":");

            if !purified
                && absorb.contains(key)
                && let Some(baseline_value) =
                    baseline.get(key).filter(|candidate| !candidate.is_empty())
            {
                joined = format!("{joined}:{baseline_value}");
            }
            record_change(&mut changes, key, Some(joined));
        }

        Self { changes }
    }

    pub fn render_shell(&self, shell: &dyn ShellOutput) -> String {
        let mut output = String::new();
        for (key, change) in &self.changes {
            match change.as_deref() {
                Some(value) => output.push_str(&shell.set_env(key, value)),
                None => output.push_str(&shell.unset_env(key)),
            }
        }
        output
    }

    pub fn contains(&self, key: &str) -> bool {
        self.changes.contains_key(key)
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.changes.keys().map(String::as_str)
    }

    pub fn record(&mut self, key: &str, value: Option<String>) {
        record_change(&mut self.changes, key, value);
    }

    pub fn to_json(&self) -> String {
        format!(
            "{}\n",
            serde_json::to_string(&self.changes).expect("env diff serializes")
        )
    }
}

pub fn live_ambient_env() -> BTreeMap<String, String> {
    vars()
        .filter(|pair| !pair.0.starts_with("__CADE_"))
        .collect()
}

pub fn is_shell_managed(key: &str) -> bool {
    matches!(key, "PWD" | "OLDPWD" | "SHLVL" | "_" | "LAST_EXIT_CODE") || key.starts_with("__CADE_")
}

fn is_pure_preserved_key(key: &str) -> bool {
    is_shell_managed(key)
        || matches!(
            key,
            "HOME"
                | "CADE_VERBOSITY"
                | "CADE_LONG_RUNNING_WARNING_MS"
                | "CADE_SHELL_GC_ROOT_TTL_SECONDS"
                | "CADE_CLIENT_ID"
        )
}

fn record_change(changes: &mut EnvDiff, key: &str, value: Option<String>) {
    if shells::is_valid_key(key) {
        changes.insert(key.to_owned(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EnvDelta,
        EnvDiff,
        is_pure_preserved_key,
        is_shell_managed,
    };

    #[test]
    fn shell_managed_classification() {
        for key in [
            "PWD",
            "OLDPWD",
            "SHLVL",
            "_",
            "LAST_EXIT_CODE",
            "__CADE_PREV",
            "__CADE_SET",
        ] {
            assert!(is_shell_managed(key), "{key} should be shell-managed");
        }
        for key in ["PATH", "HOME", "MY_VAR"] {
            assert!(!is_shell_managed(key), "{key} should not be shell-managed");
        }
        assert!(is_pure_preserved_key("HOME"));
    }

    #[test]
    fn json_escapes_separators() {
        let delta = EnvDelta {
            changes: EnvDiff::from([
                ("A".to_owned(), Some("x\x1fy".to_owned())),
                ("B".to_owned(), None),
            ]),
        };
        let out = delta.to_json();

        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["A"], "x\x1fy");
        assert!(parsed["B"].is_null());
    }
}
