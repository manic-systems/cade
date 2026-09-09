use std::path::{
    Path,
    PathBuf,
};

use anyhow::{
    Context as _,
    Result,
    anyhow,
};

use crate::{
    core::{
        Cade,
        sessions::gc_roots::nix_profile_path,
    },
    env::set::EnvSet,
    envrc::{
        envrc_arg,
        load::{
            activate_envrc,
            load_envrc,
        },
        watch::envrc_watch_files,
    },
    loaders::{
        call::call,
        env_file::load_env,
    },
    nix::{
        develop::{
            load_flake,
            load_shell,
        },
        target::{
            FlakeTarget,
            flake_watch_files,
            resolve_flake_target,
        },
    },
    types::{
        keyword::{
            Keyword,
            Loadable,
        },
        layer::{
            CachedLayer,
            CadeAction,
            CadeLayer,
        },
        load_spec::LoadSpec,
    },
};

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
        match *action {
            CadeAction::Purify => self.purify = true,
            CadeAction::Environ(ref env) => self.merge_env(env.clone()),
            CadeAction::EnvFile(ref file) => {
                self.merge_env(load_env(file).context("loading env file")?);
            },
            CadeAction::NixDevEnv(ref dev_env) => self.merge_env(dev_env.activate()?),
            CadeAction::Envrc(ref actions) => self.merge_env(activate_envrc(actions)?),
            CadeAction::Hook(ref hook) => self.hooks.push(hook.clone()),
            CadeAction::Clear(ref vars) => self.clears.extend(vars.iter().cloned()),
            CadeAction::Concat(ref vars) => self.concat.extend(vars.iter().cloned()),
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

pub(super) enum LoadRun {
    Flake(FlakeTarget),
    Shell(PathBuf),
    Env(PathBuf),
    Envrc(PathBuf),
}

pub(super) struct ResolvedLoad {
    pub run:   LoadRun,
    pub spec:  LoadSpec,
    pub watch: Vec<PathBuf>,
}

impl Loadable {
    fn file_arg(&self) -> Option<&str> {
        match *self {
            Self::Shell(ref shell_file) => {
                Some(if shell_file.is_empty() {
                    "./shell.nix"
                } else {
                    shell_file
                })
            },
            Self::Env(ref env_file) => {
                Some(if env_file.is_empty() {
                    ".env"
                } else {
                    env_file
                })
            },
            Self::Envrc(ref envrc_file) => Some(envrc_arg(envrc_file)),
            Self::Default | Self::Flake(_) => None,
        }
    }

    pub(super) fn resolve(&self, layer_dir: &Path) -> ResolvedLoad {
        use crate::path_resolve::resolve_for_watch;
        match *self {
            Self::Default | Self::Flake(_) => {
                let arg = match *self {
                    Self::Flake(ref flake_arg) => Some(flake_arg.as_str()),
                    Self::Default | Self::Shell(_) | Self::Env(_) | Self::Envrc(_) => None,
                };

                let target = resolve_flake_target(layer_dir, arg);
                let watch = flake_watch_files(&target.cwd);
                ResolvedLoad {
                    spec: target.spec.clone(),
                    watch,
                    run: LoadRun::Flake(target),
                }
            },
            Self::Shell(_) => {
                let file = resolve_for_watch(layer_dir, self.file_arg().unwrap());
                ResolvedLoad {
                    spec:  LoadSpec::Shell(file.clone()),
                    watch: vec![file.clone()],
                    run:   LoadRun::Shell(file),
                }
            },
            Self::Env(_) => {
                let file = resolve_for_watch(layer_dir, self.file_arg().unwrap());
                ResolvedLoad {
                    spec:  LoadSpec::Env(file.clone()),
                    watch: vec![file.clone()],
                    run:   LoadRun::Env(file),
                }
            },
            Self::Envrc(_) => {
                let path = resolve_for_watch(layer_dir, self.file_arg().unwrap());
                let watch = envrc_watch_files(&path);
                ResolvedLoad {
                    spec: LoadSpec::Envrc(path.clone()),
                    watch,
                    run: LoadRun::Envrc(path),
                }
            },
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
        let action = match *keyword {
            Keyword::Pure => CadeAction::Purify,
            Keyword::Call(ref raw) => {
                CadeAction::Environ(call(path, tokenize_args(raw)?).context("calling process")?)
            },
            Keyword::Load(ref loadable) => {
                let resolved = loadable.resolve(path);
                let spec_key = resolved.spec.cache_key();
                let profile =
                    nix_profile_path(cade, session, layer_count, action_index, path, &spec_key);
                let (loaded_action, env) = match resolved.run {
                    LoadRun::Flake(target) => {
                        let (dev_env, env) =
                            load_flake(&target, &profile.context("creating nix profile")?)
                                .context("loading flake")?;
                        (CadeAction::NixDevEnv(dev_env), env)
                    },
                    LoadRun::Shell(file) => {
                        let (dev_env, env) =
                            load_shell(&file, &profile.context("creating nix profile")?)
                                .context("loading shell")?;
                        (CadeAction::NixDevEnv(dev_env), env)
                    },
                    LoadRun::Env(file) => {
                        let env = load_env(&file).context("loading env file")?;
                        (CadeAction::EnvFile(file), env)
                    },
                    LoadRun::Envrc(file) => {
                        let (envrc, env) =
                            load_envrc(&file, profile.as_deref()).context("loading .envrc")?;
                        (CadeAction::Envrc(envrc), env)
                    },
                };
                layer.merge_env(env);
                actions.push(loaded_action);
                continue;
            },
            Keyword::Hook(ref hook) => CadeAction::Hook(hook.clone()),
            Keyword::Clear(ref vars) => CadeAction::Clear(vars.clone()),
            Keyword::Concat(ref vars) => CadeAction::Concat(vars.clone()),
            Keyword::Set(ref env) => CadeAction::Environ(env.clone()),
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
    use crate::{
        env::set::EnvSet,
        types::layer::{
            CadeAction,
            CadeLayer,
        },
    };

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
