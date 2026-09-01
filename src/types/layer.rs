use super::InnerHook;
use crate::env::EnvSet;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CadeAction {
    Purify,
    Environ(EnvSet),
    NixDevEnv(NixDevEnv),
    Envrc(PreparedEnvrc),
    Hook(InnerHook),
    Clear(Vec<String>),
    Concat(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedEnvrc {
    pub actions: Vec<EnvrcAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EnvrcAction {
    Environ(EnvSet),
    NixDevEnv(NixDevEnv),
    PrependPath(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NixDevEnv {
    pub script: String,
    pub cwd: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CadeLayer {
    pub envs: EnvSet,
    pub hooks: Vec<InnerHook>,
    pub purify: bool,
    pub clears: HashSet<String>,
    #[serde(default)]
    pub concat: HashSet<String>,
    #[serde(skip)]
    pub nix_store_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entry_actions: Vec<CadeAction>,
}
