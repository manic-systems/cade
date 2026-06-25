use super::InnerHook;
use crate::env::EnvSet;

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
