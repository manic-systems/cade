use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
pub enum CadeAction {
    Purify,
    Environ(EnvSet),
    Hook(InnerHook),
    Clear(Vec<String>),
    /// Mark variables as list-like (concatenating) for this layer and inward
    Concat(Vec<String>),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CadeLayer {
    pub envs: EnvSet,
    pub hooks: Vec<InnerHook>,
    pub purify: bool,
    pub clears: HashSet<String>,
    #[serde(default)]
    pub concat: HashSet<String>,
    #[serde(default)]
    pub nix_store_paths: Vec<String>,
}

#[derive(Debug)]
pub enum Keyword {
    Pure,
    /// stop the `.cade` cascade at this dir; nothing above it loads
    Disinherit,
    Call(Vec<String>),
    Load(Loadable),
    Hook(InnerHook),
    Clear(Vec<String>),
    Watch(Vec<String>),
    Concat(Vec<String>),
    Set(EnvSet),
}

#[derive(Debug)]
pub enum Loadable {
    Default,
    Flake(String),
    Shell(String),
    Env(String),
    Envrc(String),
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize)]
pub enum HookType {
    LoadPre,
    LoadPost,
    UnloadPre,
    UnloadPost,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InnerHook {
    /// Raw command text, run verbatim
    pub content: String,
    pub kind: HookType,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnvSet {
    /// Variable -> colon-split values
    pub vars: HashMap<String, Vec<String>>,
    /// Keys assigned with := hard replace
    #[serde(default)]
    pub hard: HashSet<String>,
    /// Nix store paths discovered while loading this environment.
    #[serde(default)]
    pub nix_store_paths: Vec<String>,
}

impl EnvSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from plain var -> values with no hard-replace
    pub fn from_vars(vars: HashMap<String, Vec<String>>) -> Self {
        Self {
            vars,
            hard: HashSet::new(),
            nix_store_paths: Vec::new(),
        }
    }
}
