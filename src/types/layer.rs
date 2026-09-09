use crate::env::set::EnvSet;
use crate::types::hook::InnerHook;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::PathBuf};

#[derive(Debug, Serialize, Deserialize)]
pub enum CadeAction {
    Purify,
    Environ(EnvSet),
    EnvFile(PathBuf),
    NixDevEnv(NixDevEnv),
    Envrc(Vec<EnvrcAction>),
    Hook(InnerHook),
    Clear(Vec<String>),
    Concat(Vec<String>),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CachedLayer {
    pub actions: Vec<CadeAction>,
    pub nix_store_paths: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum EnvrcAction {
    Environ(EnvSet),
    Dotenv { path: PathBuf, if_exists: bool },
    NixDevEnv(NixDevEnv),
    PrependPath(Vec<String>),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NixDevEnv {
    pub store_path: PathBuf,
    pub cwd: PathBuf,
}

#[derive(Debug, Default)]
pub struct CadeLayer {
    pub envs: EnvSet,
    pub hooks: Vec<InnerHook>,
    pub purify: bool,
    pub clears: BTreeSet<String>,
    pub concat: BTreeSet<String>,
    pub nix_store_paths: Vec<String>,
}
