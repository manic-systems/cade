use std::{fmt, str::FromStr, sync::OnceLock};

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
            "0" | "quiet" | "silent" | "none" => Ok(Verbosity::Quiet),
            "1" | "normal" | "lifecycle" | "default" => Ok(Verbosity::Normal),
            "2" | "vars" | "variables" => Ok(Verbosity::Vars),
            "3" | "trace" | "debug" | "all" => Ok(Verbosity::Trace),
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
            std::env::var("CADE_VERBOSITY")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .or_else(|| crate::config::current().verbosity)
        .unwrap_or(Verbosity::Normal)
}

pub fn enabled(level: Verbosity) -> bool {
    current() >= level
}

pub fn log(level: Verbosity, args: fmt::Arguments<'_>) {
    if enabled(level) {
        // Route through the spinner so messages never collide with a live render.
        crate::progress::log_line(&args.to_string());
    }
}
