use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LoadSpec {
    FlakeDefault,
    FlakeOutput(String),
    FlakeInstallable(String),
    Shell(PathBuf),
    Env(PathBuf),
    Envrc(PathBuf),
}

impl LoadSpec {
    pub fn cache_key(&self) -> String {
        match *self {
            Self::FlakeDefault => "flake".to_owned(),
            Self::FlakeOutput(ref output) => format!("flake:{output}"),
            Self::FlakeInstallable(ref installable) => format!("flake:{installable}"),
            Self::Shell(ref path) => format!("shell:{}", path.display()),
            Self::Env(ref path) => format!("env:{}", path.display()),
            Self::Envrc(ref path) => format!("envrc:{}", path.display()),
        }
    }
}
