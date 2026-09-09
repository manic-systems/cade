use crate::verbosity::Verbosity;
use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use std::{
    env::{var, var_os},
    fs::{canonicalize, read_to_string},
    io::ErrorKind,
    path::{Path, PathBuf},
    str::FromStr,
    sync::OnceLock,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DirenvMode {
    None,
    Shim,
    #[default]
    Envrc,
    Full,
}

impl DirenvMode {
    pub const fn loads_envrc(self) -> bool {
        matches!(self, Self::Envrc | Self::Full)
    }

    pub const fn runs_shim(self) -> bool {
        matches!(self, Self::Shim | Self::Full)
    }
}

impl FromStr for DirenvMode {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_lowercase().as_str() {
            "none" => Ok(Self::None),
            "shim" => Ok(Self::Shim),
            "envrc" => Ok(Self::Envrc),
            "full" => Ok(Self::Full),
            _ => Err(format!("unknown direnv mode: {raw}")),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub path: Option<PathBuf>,
    pub verbosity: Option<Verbosity>,
    pub long_running_warning_ms: Option<u64>,
    pub shell_gc_root_ttl_seconds: Option<u64>,
    pub direnv: DirenvMode,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    verbosity: Option<String>,
    long_running_warning_ms: Option<u64>,
    shell_gc_root_ttl_seconds: Option<u64>,
    direnv: Option<String>,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

pub fn set(config: Config) {
    let _ = CONFIG.set(config);
}

pub fn current() -> &'static Config {
    CONFIG.get_or_init(Config::default)
}

pub fn long_running_warning_ms() -> Option<u64> {
    var("CADE_LONG_RUNNING_WARNING_MS")
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .or_else(|| current().long_running_warning_ms)
}

pub fn shell_gc_root_ttl_seconds() -> Option<u64> {
    var("CADE_SHELL_GC_ROOT_TTL_SECONDS")
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .or_else(|| current().shell_gc_root_ttl_seconds)
}

pub fn direnv_mode() -> DirenvMode {
    var("CADE_DIRENV").map_or_else(
        |_| current().direnv,
        |raw| match raw.parse::<DirenvMode>() {
            Ok(mode) => mode,
            Err(parse_error) => {
                eprintln!("cade: ignoring CADE_DIRENV: {parse_error}");
                current().direnv
            }
        },
    )
}

fn home_config_path() -> Option<PathBuf> {
    let path = PathBuf::from(var_os("HOME")?)
        .join(".config")
        .join("cade")
        .join("config.toml");
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
    let active = var_os("__CADE_LAYERS").is_some() || var_os("__CADE_SESSION").is_some();
    if !active {
        return None;
    }

    var_os("__CADE_CONFIG_PATH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub fn load(path: Option<&Path>) -> Result<Config> {
    match path {
        Some(explicit) => {
            let config = read_config(explicit, true)?;
            Ok(config)
        }
        None => active_config_path()
            .or_else(default_config_path)
            .map_or_else(
                || Ok(Config::default()),
                |fallback| read_config(&fallback, false),
            ),
    }
}

fn read_config(path: &Path, strict: bool) -> Result<Config> {
    let text = match read_to_string(path) {
        Ok(text) => text,
        Err(read_error) if !strict && read_error.kind() == ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(read_error) => {
            return Err(read_error)
                .with_context(|| format!("reading config at {}", path.display()));
        }
    };

    let parsed: RawConfig =
        toml::from_str(&text).with_context(|| format!("parsing config at {}", path.display()))?;
    let mut config: Config = parsed.try_into()?;
    config.path = Some(canonicalize(path).unwrap_or_else(|_| path.to_path_buf()));
    Ok(config)
}

impl TryFrom<RawConfig> for Config {
    type Error = anyhow::Error;

    fn try_from(raw: RawConfig) -> Result<Self> {
        let verbosity = match raw.verbosity {
            Some(verbosity_text) => Some(
                verbosity_text
                    .parse::<Verbosity>()
                    .map_err(|parse_error| anyhow::anyhow!("{parse_error}"))?,
            ),
            None => None,
        };
        if matches!(raw.long_running_warning_ms, Some(0)) {
            bail!("long_running_warning_ms must be greater than 0");
        }
        if matches!(raw.shell_gc_root_ttl_seconds, Some(0)) {
            bail!("shell_gc_root_ttl_seconds must be greater than 0");
        }
        let direnv = match raw.direnv {
            Some(direnv_text) => direnv_text
                .parse::<DirenvMode>()
                .map_err(|parse_error| anyhow::anyhow!("{parse_error}"))?,
            None => DirenvMode::default(),
        };
        Ok(Self {
            path: None,
            verbosity,
            long_running_warning_ms: raw.long_running_warning_ms,
            shell_gc_root_ttl_seconds: raw.shell_gc_root_ttl_seconds,
            direnv,
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
            shell_gc_root_ttl_seconds: Some(200),
            direnv: Some("full".into()),
        };
        let cfg: Config = raw.try_into().unwrap();
        assert_eq!(cfg.verbosity, Some(Verbosity::Vars));
        assert_eq!(cfg.long_running_warning_ms, Some(100));
        assert_eq!(cfg.shell_gc_root_ttl_seconds, Some(200));
        assert_eq!(cfg.direnv, DirenvMode::Full);
    }

    #[test]
    fn rejects_bad_verbosity() {
        let raw = RawConfig {
            verbosity: Some("loud".into()),
            ..Default::default()
        };
        Config::try_from(raw).unwrap_err();
    }

    #[test]
    fn rejects_zero_warning_threshold() {
        let raw = RawConfig {
            long_running_warning_ms: Some(0),
            ..Default::default()
        };
        Config::try_from(raw).unwrap_err();
    }

    #[test]
    fn rejects_zero_shell_gc_root_ttl() {
        let raw = RawConfig {
            shell_gc_root_ttl_seconds: Some(0),
            ..Default::default()
        };
        Config::try_from(raw).unwrap_err();
    }

    #[test]
    fn parses_each_direnv_mode() {
        for (text, mode) in [
            ("none", DirenvMode::None),
            ("shim", DirenvMode::Shim),
            ("envrc", DirenvMode::Envrc),
            ("full", DirenvMode::Full),
        ] {
            assert_eq!(text.parse::<DirenvMode>().unwrap(), mode);
        }

        let raw = RawConfig::default();
        assert_eq!(Config::try_from(raw).unwrap().direnv, DirenvMode::Envrc);
    }

    #[test]
    fn rejects_bad_direnv_mode() {
        let raw = RawConfig {
            direnv: Some("sometimes".into()),
            ..Default::default()
        };
        Config::try_from(raw).unwrap_err();
    }

    #[test]
    fn direnv_mode_predicates() {
        assert!(!DirenvMode::None.loads_envrc() && !DirenvMode::None.runs_shim());
        assert!(!DirenvMode::Shim.loads_envrc() && DirenvMode::Shim.runs_shim());
        assert!(DirenvMode::Envrc.loads_envrc() && !DirenvMode::Envrc.runs_shim());
        assert!(DirenvMode::Full.loads_envrc() && DirenvMode::Full.runs_shim());
    }
}
