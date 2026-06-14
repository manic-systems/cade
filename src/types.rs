use crate::env::EnvSet;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug)]
pub enum CadeAction {
    Purify,
    Environ(EnvSet),
    Hook(InnerHook),
    Clear(Vec<String>),
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
    /// cold-path gc roots
    #[serde(skip)]
    pub nix_store_paths: Vec<String>,
}

#[derive(Debug)]
pub enum Keyword {
    Pure,
    Disinherit,
    Call(String),
    Load(Loadable),
    Hook(InnerHook),
    Clear(Vec<String>),
    Watch(String),
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
    pub content: String,
    pub kind: HookType,
}
