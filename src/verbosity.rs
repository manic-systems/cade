use std::{
    env::var,
    fmt,
    str::FromStr,
    sync::OnceLock,
};

use crate::{
    config::current as config_current,
    progress::log_line,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verbosity {
    Quiet,
    Normal,
    Vars,
    Trace,
}

impl FromStr for Verbosity {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_lowercase().as_str() {
            "0" | "quiet" | "silent" | "none" => Ok(Self::Quiet),
            "1" | "normal" | "lifecycle" | "default" => Ok(Self::Normal),
            "2" | "vars" | "variables" => Ok(Self::Vars),
            "3" | "trace" | "debug" | "all" => Ok(Self::Trace),
            _ => Err(format!("unknown verbosity: {raw}")),
        }
    }
}

static OVERRIDE: OnceLock<Verbosity> = OnceLock::new();

pub fn set(verbosity: Verbosity) {
    let _ = OVERRIDE.set(verbosity);
}

pub fn current() -> Verbosity {
    OVERRIDE
        .get()
        .copied()
        .or_else(|| {
            let env_value = var("CADE_VERBOSITY").ok()?;
            env_value.parse().ok()
        })
        .or_else(|| config_current().verbosity)
        .unwrap_or(Verbosity::Normal)
}

pub fn enabled(level: Verbosity) -> bool {
    current() >= level
}

pub fn log(level: Verbosity, args: fmt::Arguments<'_>) {
    if enabled(level) {
        log_line(&args.to_string());
    }
}
