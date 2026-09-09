use crate::core::Cade;
use crate::envrc::load::{activate_envrc, load_envrc};
use crate::loaders::{call, load_env};
use crate::nix::develop::{load_flake, load_shell};
use crate::types::layer::CachedLayer;
use crate::{
    env::EnvSet,
    types::{CadeAction, CadeLayer, Keyword, LoadSpec, Loadable},
};
use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};

impl CadeLayer {
    pub fn merge_env(&mut self, env: EnvSet) {
        let merged = self.envs.merge_layer_env(env);
        for key in merged.sets {
            self.clears.remove(&key);
        }
        self.clears.extend(merged.clears);
        self.nix_store_paths.extend(merged.store_paths);
    }

    pub fn push_action(&mut self, action: &CadeAction) -> Result<()> {
        match action {
            CadeAction::Purify => self.purify = true,
            CadeAction::Environ(env) => self.merge_env(env.clone()),
            CadeAction::EnvFile(file) => {
                self.merge_env(load_env(file).context("loading env file")?);
            }
            CadeAction::NixDevEnv(dev_env) => self.merge_env(dev_env.activate()?),
            CadeAction::Envrc(actions) => self.merge_env(activate_envrc(actions)?),
            CadeAction::Hook(hook) => self.hooks.push(hook.clone()),
            CadeAction::Clear(vars) => self.clears.extend(vars.iter().cloned()),
            CadeAction::Concat(vars) => self.concat.extend(vars.iter().cloned()),
        }
        Ok(())
    }
}

impl CachedLayer {
    pub(super) fn activate(&self) -> Result<CadeLayer> {
        let mut layer = CadeLayer::default();
        for action in &self.actions {
            layer.push_action(action)?;
        }
        Ok(layer)
    }
}

enum LoadRun {
    Flake(crate::nix::FlakeTarget),
    Shell(PathBuf),
    Env(PathBuf),
    Envrc(PathBuf),
}

pub(super) struct ResolvedLoad {
    run: LoadRun,
    spec: LoadSpec,
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

    pub(super) fn resolve(&self, layer_dir: &Path) -> ResolvedLoad {
        use crate::path_resolve::resolve_for_watch;
        match self {
            Loadable::Default | Loadable::Flake(_) => {
                let arg = match self {
                    Loadable::Flake(a) => Some(a.as_str()),
                    _ => None,
                };

                let target = crate::nix::resolve_flake_target(layer_dir, arg);
                let watch = crate::nix::flake_watch_files(&target.cwd);
                ResolvedLoad {
                    spec: target.spec.clone(),
                    watch,
                    run: LoadRun::Flake(target),
                }
            }
            Loadable::Shell(_) => {
                let file = resolve_for_watch(layer_dir, self.file_arg().unwrap());
                ResolvedLoad {
                    spec: LoadSpec::Shell(file.clone()),
                    watch: vec![file.clone()],
                    run: LoadRun::Shell(file),
                }
            }
            Loadable::Env(_) => {
                let file = resolve_for_watch(layer_dir, self.file_arg().unwrap());
                ResolvedLoad {
                    spec: LoadSpec::Env(file.clone()),
                    watch: vec![file.clone()],
                    run: LoadRun::Env(file),
                }
            }
            Loadable::Envrc(_) => {
                let path = resolve_for_watch(layer_dir, self.file_arg().unwrap());
                let watch = crate::envrc::envrc_watch_files(&path);
                ResolvedLoad {
                    spec: LoadSpec::Envrc(path.clone()),
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
    session: &str,
) -> Result<(CachedLayer, CadeLayer)> {
    let mut actions = Vec::new();
    let mut layer = CadeLayer::default();

    for (action_index, keyword) in keywords.iter().enumerate() {
        let action = match keyword {
            Keyword::Pure => CadeAction::Purify,
            Keyword::Call(raw) => {
                CadeAction::Environ(call(path, tokenize_args(raw)?).context("calling process")?)
            }
            Keyword::Load(loadable) => {
                let resolved = loadable.resolve(path);
                let spec_key = resolved.spec.cache_key();
                let profile =
                    cade.nix_profile_path(session, layer_count, action_index, path, &spec_key);
                let (loaded_action, env) = match resolved.run {
                    LoadRun::Flake(target) => {
                        let (dev_env, env) =
                            load_flake(&target, &profile.context("creating nix profile")?)
                                .context("loading flake")?;
                        (CadeAction::NixDevEnv(dev_env), env)
                    }
                    LoadRun::Shell(file) => {
                        let (dev_env, env) =
                            load_shell(&file, &profile.context("creating nix profile")?)
                                .context("loading shell")?;
                        (CadeAction::NixDevEnv(dev_env), env)
                    }
                    LoadRun::Env(file) => {
                        let env = load_env(&file).context("loading env file")?;
                        (CadeAction::EnvFile(file), env)
                    }
                    LoadRun::Envrc(file) => {
                        let (envrc, env) = load_envrc(&file, profile).context("loading .envrc")?;
                        (CadeAction::Envrc(envrc), env)
                    }
                };
                layer.merge_env(env);
                actions.push(loaded_action);
                continue;
            }
            Keyword::Hook(hook) => CadeAction::Hook(hook.clone()),
            Keyword::Clear(vars) => CadeAction::Clear(vars.clone()),
            Keyword::Concat(vars) => CadeAction::Concat(vars.clone()),
            Keyword::Set(env) => CadeAction::Environ(env.clone()),
            Keyword::Watch(_) | Keyword::Disinherit => continue,
        };
        layer.push_action(&action)?;
        actions.push(action);
    }

    let cached = CachedLayer {
        actions,
        nix_store_paths: layer.nix_store_paths.clone(),
    };
    Ok((cached, layer))
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
        let mut layer = CadeLayer::default();

        layer.push_action(&CadeAction::Environ(env)).unwrap();

        assert_eq!(layer.nix_store_paths, [STORE_PATH]);
        assert_eq!(layer.envs.derived_store_paths(), [STORE_PATH]);
    }
}
