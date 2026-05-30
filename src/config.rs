use crate::verbosity::Verbosity;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub path: Option<PathBuf>,
    pub verbosity: Option<Verbosity>,
    pub long_running_warning_ms: Option<u64>,
    pub shell_gc_root_ttl_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    verbosity: Option<String>,
    long_running_warning_ms: Option<u64>,
    shell_gc_root_ttl_seconds: Option<u64>,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

pub fn set(config: Config) {
    let _ = CONFIG.set(config);
}

pub fn current() -> &'static Config {
    CONFIG.get_or_init(Config::default)
}

pub fn long_running_warning_ms() -> Option<u64> {
    std::env::var("CADE_LONG_RUNNING_WARNING_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .or_else(|| current().long_running_warning_ms)
}

pub fn shell_gc_root_ttl_seconds() -> Option<u64> {
    std::env::var("CADE_SHELL_GC_ROOT_TTL_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .or_else(|| current().shell_gc_root_ttl_seconds)
}

fn home_config_path() -> Option<PathBuf> {
    let mut path = PathBuf::from(std::env::var_os("HOME")?);
    path.push(".config");
    path.push("cade");
    path.push("config.toml");
    Some(path)
}

pub fn default_config_path() -> Option<PathBuf> {
    microxdg::XdgApp::new("cade")
        .ok()
        .and_then(|app| app.app_config().ok())
        .map(|mut path| {
            path.push("config.toml");
            path
        })
        .or_else(home_config_path)
}

fn active_config_path() -> Option<PathBuf> {
    let active =
        std::env::var_os("__CADE_LAYERS").is_some() || std::env::var_os("__CADE_SESSION").is_some();
    if !active {
        return None;
    }

    std::env::var_os("__CADE_CONFIG_PATH")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

pub fn load(path: Option<&Path>) -> Result<Config> {
    match path {
        Some(path) => {
            let config = read_config(path, true)?;
            Ok(config)
        }
        None => {
            if let Some(path) = active_config_path().or_else(default_config_path) {
                read_config(&path, false)
            } else {
                Ok(Config::default())
            }
        }
    }
}

fn read_config(path: &Path, strict: bool) -> Result<Config> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if !strict && e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(e) => return Err(e).with_context(|| format!("reading config at {}", path.display())),
    };

    let raw: RawConfig =
        toml::from_str(&raw).with_context(|| format!("parsing config at {}", path.display()))?;
    let mut config: Config = raw.try_into()?;
    config.path = Some(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()));
    Ok(config)
}

impl TryFrom<RawConfig> for Config {
    type Error = anyhow::Error;

    fn try_from(raw: RawConfig) -> Result<Self> {
        let verbosity = match raw.verbosity {
            Some(v) => Some(v.parse::<Verbosity>().map_err(|e| anyhow::anyhow!("{e}"))?),
            None => None,
        };
        if matches!(raw.long_running_warning_ms, Some(0)) {
            bail!("long_running_warning_ms must be greater than 0");
        }
        if matches!(raw.shell_gc_root_ttl_seconds, Some(0)) {
            bail!("shell_gc_root_ttl_seconds must be greater than 0");
        }
        Ok(Self {
            path: None,
            verbosity,
            long_running_warning_ms: raw.long_running_warning_ms,
            shell_gc_root_ttl_seconds: raw.shell_gc_root_ttl_seconds,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_config() {
        let raw = RawConfig {
            verbosity: Some("vars".into()),
            long_running_warning_ms: Some(100),
            shell_gc_root_ttl_seconds: Some(60),
        };
        let cfg: Config = raw.try_into().unwrap();
        assert_eq!(cfg.verbosity, Some(Verbosity::Vars));
        assert_eq!(cfg.long_running_warning_ms, Some(100));
        assert_eq!(cfg.shell_gc_root_ttl_seconds, Some(60));
    }

    #[test]
    fn rejects_bad_verbosity() {
        let raw = RawConfig {
            verbosity: Some("loud".into()),
            long_running_warning_ms: None,
            shell_gc_root_ttl_seconds: None,
        };
        assert!(Config::try_from(raw).is_err());
    }

    #[test]
    fn rejects_zero_warning_threshold() {
        let raw = RawConfig {
            verbosity: None,
            long_running_warning_ms: Some(0),
            shell_gc_root_ttl_seconds: None,
        };
        assert!(Config::try_from(raw).is_err());
    }

    #[test]
    fn rejects_zero_shell_gc_root_ttl() {
        let raw = RawConfig {
            verbosity: None,
            long_running_warning_ms: None,
            shell_gc_root_ttl_seconds: Some(0),
        };
        assert!(Config::try_from(raw).is_err());
    }
}
