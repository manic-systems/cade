use super::{Lookup, eval::expand_plain, quote::expand_shell_args};
use crate::{
    env::EnvSet,
    types::{Keyword, Loadable},
};

pub fn expand_keyword(kw: &mut Keyword) {
    expand_keyword_with(kw, &|k| std::env::var(k).ok());
}

fn expand_keyword_with(kw: &mut Keyword, lookup: Lookup<'_>) {
    use Keyword::*;
    match kw {
        Call(s) | Watch(s) => *s = expand_shell_args(s, lookup),
        Load(loadable) => expand_loadable(loadable, lookup),
        Set(env) => expand_envset(env, lookup),
        Hook(_) | Clear(_) | Concat(_) | Pure | Disinherit => {}
    }
}

fn expand_loadable(loadable: &mut Loadable, lookup: Lookup<'_>) {
    use Loadable::*;
    match loadable {
        Flake(s) | Shell(s) | Env(s) | Envrc(s) => *s = expand_plain(s, lookup),
        Default => {}
    }
}

fn expand_envset(env: &mut EnvSet, lookup: Lookup<'_>) {
    env.expand_values(|value| expand_plain(value, lookup));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    fn env_values(env: &EnvSet, key: &str) -> Vec<String> {
        serde_json::to_value(env).unwrap()["vars"][key]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn walks_call_and_load_but_not_hook() {
        let lookup = lookup_from(&[("TOKEN", "secret"), ("DIR", "/srv")]);
        let mut call = Keyword::Call("tool --t=${TOKEN}".into());
        expand_keyword_with(&mut call, &lookup);
        match call {
            Keyword::Call(s) => {
                assert_eq!(shlex::split(&s).unwrap(), vec!["tool", "--t=secret"])
            }
            other => panic!("expected Call, got {other:?}"),
        }

        let mut load = Keyword::Load(Loadable::Env("${DIR}/.env".into()));
        expand_keyword_with(&mut load, &lookup);
        match load {
            Keyword::Load(Loadable::Env(p)) => assert_eq!(p, "/srv/.env"),
            other => panic!("expected Load env, got {other:?}"),
        }

        let mut hook = Keyword::Hook(crate::types::InnerHook {
            kind: crate::types::HookType::LoadPost,
            content: "echo ${TOKEN}".into(),
        });
        expand_keyword_with(&mut hook, &lookup);
        match hook {
            Keyword::Hook(h) => assert_eq!(h.content, "echo ${TOKEN}"),
            other => panic!("expected Hook, got {other:?}"),
        }
    }

    #[test]
    fn walks_inline_assignment_with_colon_dash_default() {
        let lookup = lookup_from(&[]);
        let mut set = "MODE=${MODE:-dev}".parse::<Keyword>().unwrap();
        expand_keyword_with(&mut set, &lookup);
        match set {
            Keyword::Set(env) => assert_eq!(env_values(&env, "MODE"), vec!["dev"]),
            other => panic!("expected Set, got {other:?}"),
        }
    }

    #[test]
    fn walks_inline_assignment_value_keeping_colon_lists() {
        let lookup = lookup_from(&[("EXTRA", "/a:/b")]);
        let mut set = "MYPATH=${EXTRA}:/c".parse::<Keyword>().unwrap();
        expand_keyword_with(&mut set, &lookup);
        match set {
            Keyword::Set(env) => assert_eq!(env_values(&env, "MYPATH"), vec!["/a", "/b", "/c"]),
            other => panic!("expected Set, got {other:?}"),
        }
    }

    #[test]
    fn inline_assignment_expansion_refreshes_store_paths() {
        const STORE_PATH: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-expanded";

        let lookup = lookup_from(&[("TOOL", STORE_PATH)]);
        let mut set = "TOOL=${TOOL}".parse::<Keyword>().unwrap();
        expand_keyword_with(&mut set, &lookup);

        match set {
            Keyword::Set(env) => assert_eq!(env.derived_store_paths(), [STORE_PATH]),
            other => panic!("expected Set, got {other:?}"),
        }
    }
}
