use super::Cade;
use crate::{
    env::EnvSet,
    types::{CadeAction, CadeLayer, Keyword, Loadable},
};
use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};

impl CadeLayer {
    pub fn new(_layer: usize, _origin: &Path) -> Self {
        Self {
            envs: EnvSet::new(),
            hooks: Vec::new(),
            purify: false,
            clears: std::collections::HashSet::new(),
            concat: std::collections::HashSet::new(),
            nix_store_paths: Vec::new(),
        }
    }

    pub fn push_action(&mut self, action: CadeAction) {
        use CadeAction::*;
        match action {
            Purify => {
                self.purify = true;
            }
            Environ(env) => {
                self.nix_store_paths.extend(self.envs.merge_layer_env(env));
            }
            Hook(hook) => {
                self.hooks.push(hook);
            }
            Clear(vars) => {
                self.clears.extend(vars);
            }
            Concat(vars) => {
                self.concat.extend(vars);
            }
        }
    }
}

enum LoadRun {
    Flake(crate::nix_dev_env::FlakeTarget),
    Shell(PathBuf),
    Env(PathBuf),
    Envrc(PathBuf),
}

pub(super) struct ResolvedLoad {
    run: LoadRun,
    spec: String,
    pub(super) watch: Vec<PathBuf>,
}

impl Loadable {
    fn file_arg(&self) -> Option<&str> {
        match self {
            Loadable::Shell(f) => Some(if f.is_empty() { "./shell.nix" } else { f }),
            Loadable::Env(f) => Some(if f.is_empty() { ".env" } else { f }),
            Loadable::Envrc(f) => Some(crate::envrc::envrc_arg(f)),
            Loadable::Default | Loadable::Flake(_) => None,
        }
    }

    // one target owns loading and watching
    pub(super) fn resolve(&self, layer_dir: &Path) -> ResolvedLoad {
        use crate::path_resolve::resolve_for_watch;
        match self {
            Loadable::Default | Loadable::Flake(_) => {
                let arg = match self {
                    Loadable::Flake(a) => Some(a.as_str()),
                    _ => None,
                };
                // let nix report missing flake dirs
                let target = crate::nix_dev_env::resolve_flake_target(layer_dir, arg);
                let watch = vec![target.cwd.join("flake.nix"), target.cwd.join("flake.lock")];
                ResolvedLoad {
                    spec: target.spec.clone(),
                    watch,
                    run: LoadRun::Flake(target),
                }
            }
            Loadable::Shell(_) => {
                let file = resolve_for_watch(layer_dir, self.file_arg().unwrap());
                ResolvedLoad {
                    spec: format!("shell:{}", file.display()),
                    watch: vec![file.clone()],
                    run: LoadRun::Shell(file),
                }
            }
            Loadable::Env(_) => {
                let file = resolve_for_watch(layer_dir, self.file_arg().unwrap());
                ResolvedLoad {
                    spec: format!("env:{}", file.display()),
                    watch: vec![file.clone()],
                    run: LoadRun::Env(file),
                }
            }
            Loadable::Envrc(_) => {
                let path = resolve_for_watch(layer_dir, self.file_arg().unwrap());
                let watch = crate::envrc::envrc_watch_files(&path);
                ResolvedLoad {
                    spec: format!("envrc:{}", path.display()),
                    watch,
                    run: LoadRun::Envrc(path),
                }
            }
        }
    }
}

pub(super) fn load_single_layer(
    layer_count: usize,
    path: &Path,
    keywords: &[Keyword],
    cade: &Cade,
    session: Option<&str>,
) -> Result<CadeLayer> {
    use crate::loaders::*;
    use Keyword::*;

    let mut layer = CadeLayer::new(layer_count, path);
    for (action_index, kw) in keywords.iter().enumerate() {
        let act = match kw {
            Pure => Ok(CadeAction::Purify),
            Call(raw) => call(path, tokenize_args(raw)?)
                .context("calling process")
                .map(CadeAction::Environ),
            Load(loadable) => {
                let resolved = loadable.resolve(path);
                let profile = session.and_then(|session| {
                    cade.nix_profile_path(session, layer_count, action_index, path, &resolved.spec)
                });
                match resolved.run {
                    LoadRun::Flake(target) => load_flake(&target, profile).context("loading flake"),
                    LoadRun::Shell(file) => load_shell(&file, profile).context("loading shell"),
                    LoadRun::Env(file) => load_env(&file).context("loading env file"),
                    LoadRun::Envrc(p) => {
                        crate::envrc::load_envrc(&p, profile).context("loading .envrc")
                    }
                }
                .map(CadeAction::Environ)
            }
            Hook(hook) => Ok(CadeAction::Hook(hook.clone())),
            Clear(vars) => Ok(CadeAction::Clear(vars.clone())),
            Concat(vars) => Ok(CadeAction::Concat(vars.clone())),
            Set(env) => Ok(CadeAction::Environ(env.clone())),
            // chain only
            Watch(_) | Disinherit => continue,
        }?;
        layer.push_action(act);
    }
    Ok(layer)
}

pub(super) fn tokenize_args(raw: &str) -> Result<Vec<String>> {
    shlex::split(raw).ok_or_else(|| anyhow!("unbalanced quotes in `{raw}`"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{env::EnvSet, types::CadeAction};

    const STORE_PATH: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-layer";

    #[test]
    fn layer_merge_preserves_store_path_metadata() {
        let env = EnvSet::from_envs(&format!("TOOL={STORE_PATH}\n")).unwrap();
        let mut layer = CadeLayer::new(0, Path::new("/"));

        layer.push_action(CadeAction::Environ(env));

        assert_eq!(layer.nix_store_paths, [STORE_PATH]);
        assert_eq!(layer.envs.derived_store_paths(), [STORE_PATH]);
    }
}
