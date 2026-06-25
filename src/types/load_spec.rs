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
        match self {
            LoadSpec::FlakeDefault => "flake".to_string(),
            LoadSpec::FlakeOutput(output) => format!("flake:{output}"),
            LoadSpec::FlakeInstallable(installable) => format!("flake:{installable}"),
            LoadSpec::Shell(path) => format!("shell:{}", path.display()),
            LoadSpec::Env(path) => format!("env:{}", path.display()),
            LoadSpec::Envrc(path) => format!("envrc:{}", path.display()),
        }
    }
}
