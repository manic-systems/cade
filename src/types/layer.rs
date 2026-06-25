use super::InnerHook;
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
    #[serde(skip)]
    pub nix_store_paths: Vec<String>,
}
