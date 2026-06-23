use super::{WatchState, is_valid_session};
use crate::{shells::ShellOutput, types::InnerHook};
use std::path::PathBuf;

const KEY_SEPARATOR: &str = "\x1F";

pub(super) const SESSION_VAR: &str = "__CADE_SESSION";
pub(super) const LAYERS_VAR: &str = "__CADE_LAYERS";
pub(super) const STATE_DIR_VAR: &str = "__CADE_STATE_DIR";
pub(super) const CONFIG_PATH_VAR: &str = "__CADE_CONFIG_PATH";
pub const SET_VAR: &str = "__CADE_SET";
pub(super) const UNSET_VAR: &str = "__CADE_UNSET";
pub(super) const PURE_VAR: &str = "__CADE_PURE";
pub(super) const HOOKS_VAR: &str = "__CADE_HOOKS";
pub(super) const WATCHES_VAR: &str = "__CADE_WATCHES";

const ACTIVATION_VARS: &[&str] = &[
    LAYERS_VAR,
    SET_VAR,
    UNSET_VAR,
    PURE_VAR,
    WATCHES_VAR,
    HOOKS_VAR,
    STATE_DIR_VAR,
    CONFIG_PATH_VAR,
];

pub(super) struct ShellState {
    layers_present: bool,
    session: Option<String>,
    layers: Vec<PathBuf>,
    state_dir: Option<PathBuf>,
    config_path: Option<PathBuf>,
    set_keys: Vec<String>,
    set_present: bool,
    unset_keys: Vec<String>,
    pure: bool,
    hooks: Vec<InnerHook>,
    watches: Option<WatchState>,
}

impl ShellState {
    pub(super) fn from_env() -> Self {
        let layers = std::env::var(LAYERS_VAR).ok();
        let set = std::env::var(SET_VAR).ok();
        Self {
            layers_present: layers.is_some(),
            session: std::env::var(SESSION_VAR).ok(),
            layers: layers.as_deref().map(decode_path_list).unwrap_or_default(),
            state_dir: std::env::var(STATE_DIR_VAR).ok().map(PathBuf::from),
            config_path: std::env::var(CONFIG_PATH_VAR).ok().map(PathBuf::from),
            set_keys: set.as_deref().map(decode_key_list).unwrap_or_default(),
            set_present: set.is_some(),
            unset_keys: std::env::var(UNSET_VAR)
                .ok()
                .as_deref()
                .map(decode_key_list)
                .unwrap_or_default(),
            pure: std::env::var(PURE_VAR).map(|v| v == "1").unwrap_or(false),
            hooks: std::env::var(HOOKS_VAR)
                .ok()
                .and_then(|h| serde_json::from_str(&h).ok())
                .unwrap_or_default(),
            watches: std::env::var(WATCHES_VAR)
                .ok()
                .and_then(|w| serde_json::from_str(&w).ok()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn active(
        session: String,
        layers: Vec<PathBuf>,
        state_dir: PathBuf,
        config_path: Option<PathBuf>,
        set_keys: Vec<String>,
        unset_keys: Vec<String>,
        pure: bool,
        hooks: Vec<InnerHook>,
        watches: WatchState,
    ) -> Self {
        Self {
            layers_present: true,
            session: Some(session),
            layers,
            state_dir: Some(state_dir),
            config_path,
            set_present: true,
            set_keys,
            unset_keys,
            pure,
            hooks,
            watches: Some(watches),
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        !self.layers_present && self.session.is_none() && !self.set_present
    }

    pub(super) fn is_active(&self) -> bool {
        self.layers_present
    }

    pub(super) fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    pub(super) fn valid_session(&self) -> Option<&str> {
        self.session().filter(|session| is_valid_session(session))
    }

    pub(super) fn set_keys(&self) -> &[String] {
        &self.set_keys
    }

    pub(super) fn unset_keys(&self) -> &[String] {
        &self.unset_keys
    }

    pub(super) fn pure(&self) -> bool {
        self.pure
    }

    pub(super) fn hooks(&self) -> &[InnerHook] {
        &self.hooks
    }

    pub(super) fn watch_state(&self) -> Option<&WatchState> {
        self.watches.as_ref()
    }

    pub(super) fn unload_summary(&self) -> Option<(String, usize)> {
        self.layers
            .last()
            .map(|tip| (tip.to_string_lossy().to_string(), self.layers.len().max(1)))
    }

    pub(super) fn render_activation(&self, shell: &dyn ShellOutput, new_session: bool) -> String {
        let mut out = String::new();
        if new_session && let Some(session) = &self.session {
            out.push_str(&shell.set_env(SESSION_VAR, session));
        }
        out.push_str(&shell.set_env(LAYERS_VAR, &encode_paths(&self.layers)));
        if let Some(state_dir) = &self.state_dir {
            out.push_str(&shell.set_env(STATE_DIR_VAR, &state_dir.to_string_lossy()));
        }
        if let Some(config_path) = &self.config_path {
            out.push_str(&shell.set_env(CONFIG_PATH_VAR, &config_path.to_string_lossy()));
        }
        out.push_str(&shell.set_env(SET_VAR, &encode_key_list(&self.set_keys)));
        out.push_str(&shell.set_env(UNSET_VAR, &encode_key_list(&self.unset_keys)));
        out.push_str(&shell.set_env(PURE_VAR, if self.pure { "1" } else { "0" }));
        out.push_str(&shell.set_env(
            HOOKS_VAR,
            &serde_json::to_string(&self.hooks).unwrap_or_default(),
        ));
        if let Some(watches) = &self.watches {
            out.push_str(&shell.set_env(
                WATCHES_VAR,
                &serde_json::to_string(watches).unwrap_or_default(),
            ));
        }
        out
    }

    pub(super) fn render_clear(&self, shell: &dyn ShellOutput, finalise: bool) -> String {
        let mut out = String::new();
        for var in ACTIVATION_VARS {
            out.push_str(&shell.unset_env(var));
        }
        if finalise {
            out.push_str(&shell.unset_env(SESSION_VAR));
        }
        out
    }
}

pub fn decode_key_list(raw: &str) -> Vec<String> {
    raw.split(KEY_SEPARATOR)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

fn decode_path_list(raw: &str) -> Vec<PathBuf> {
    decode_key_list(raw)
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

fn encode_key_list(keys: &[String]) -> String {
    keys.join(KEY_SEPARATOR)
}

fn encode_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(KEY_SEPARATOR)
}

pub(super) fn state_dir_from_env() -> Option<PathBuf> {
    std::env::var(STATE_DIR_VAR).ok().map(PathBuf::from)
}
