use std::env::var;

use super::{
    Lookup,
    eval::expand_plain,
    quote::expand_shell_args,
};
use crate::{
    env::set::EnvSet,
    types::keyword::{
        Keyword,
        Loadable,
    },
};

pub fn expand_keyword(kw: &mut Keyword) {
    expand_keyword_with(kw, &|key| var(key).ok());
}

fn expand_keyword_with(kw: &mut Keyword, lookup: Lookup<'_>) {
    use Keyword::{
        Call,
        Clear,
        Concat,
        Disinherit,
        Hook,
        Load,
        Pure,
        Set,
        Watch,
    };
    match *kw {
        Call(ref mut command) | Watch(ref mut command) => {
            *command = expand_shell_args(command, lookup);
        },
        Load(ref mut loadable) => expand_loadable(loadable, lookup),
        Set(ref mut env) => expand_envset(env, lookup),
        Hook(_) | Clear(_) | Concat(_) | Pure | Disinherit => {},
    }
}

fn expand_loadable(loadable: &mut Loadable, lookup: Lookup<'_>) {
    use Loadable::{
        Default,
        Env,
        Envrc,
        Flake,
        Shell,
    };
    match *loadable {
        Flake(ref mut source)
        | Shell(ref mut source)
        | Env(ref mut source)
        | Envrc(ref mut source) => {
            *source = expand_plain(source, lookup);
        },
        Default => {},
    }
}

fn expand_envset(env: &mut EnvSet, lookup: Lookup<'_>) {
    env.expand_values(|value| expand_plain(value, lookup));
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::types::hook::{
        HookType,
        InnerHook,
    };

    fn lookup_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|&(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    fn env_values(env: &EnvSet, key: &str) -> Vec<String> {
        serde_json::to_value(env).unwrap()["vars"][key]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn walks_call_and_load_but_not_hook() {
        let lookup = lookup_from(&[("TOKEN", "secret"), ("DIR", "/srv")]);
        let mut call = Keyword::Call("tool --t=${TOKEN}".into());
        expand_keyword_with(&mut call, &lookup);
        assert!(matches!(call, Keyword::Call(_)));
        if let Keyword::Call(command) = call {
            assert_eq!(shlex::split(&command).unwrap(), vec!["tool", "--t=secret"]);
        }

        let mut load = Keyword::Load(Loadable::Env("${DIR}/.env".into()));
        expand_keyword_with(&mut load, &lookup);
        assert!(matches!(load, Keyword::Load(Loadable::Env(_))));
        if let Keyword::Load(Loadable::Env(path)) = load {
            assert_eq!(path, "/srv/.env");
        }

        let mut hook = Keyword::Hook(InnerHook {
            kind:    HookType::LoadPost,
            content: "echo ${TOKEN}".into(),
        });
        expand_keyword_with(&mut hook, &lookup);
        assert!(matches!(hook, Keyword::Hook(_)));
        if let Keyword::Hook(inner_hook) = hook {
            assert_eq!(inner_hook.content, "echo ${TOKEN}");
        }
    }

    #[test]
    #[expect(
        clippy::literal_string_with_formatting_args,
        reason = "Shell parameter expansion is input to the parser"
    )]
    fn walks_inline_assignment_with_colon_dash_default() {
        let lookup = lookup_from(&[]);
        let mut set = "MODE=${MODE:-dev}".parse::<Keyword>().unwrap();
        expand_keyword_with(&mut set, &lookup);
        assert!(matches!(set, Keyword::Set(_)));
        if let Keyword::Set(env) = set {
            assert_eq!(env_values(&env, "MODE"), vec!["dev"]);
        }
    }

    #[test]
    fn walks_inline_assignment_value_keeping_colon_lists() {
        let lookup = lookup_from(&[("EXTRA", "/a:/b")]);
        let mut set = "MYPATH=${EXTRA}:/c".parse::<Keyword>().unwrap();
        expand_keyword_with(&mut set, &lookup);
        assert!(matches!(set, Keyword::Set(_)));
        if let Keyword::Set(env) = set {
            assert_eq!(env_values(&env, "MYPATH"), vec!["/a", "/b", "/c"]);
        }
    }

    #[test]
    fn inline_assignment_expansion_refreshes_store_paths() {
        const STORE_PATH: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-expanded";

        let lookup = lookup_from(&[("TOOL", STORE_PATH)]);
        let mut set = "TOOL=${TOOL}".parse::<Keyword>().unwrap();
        expand_keyword_with(&mut set, &lookup);

        assert!(matches!(set, Keyword::Set(_)));
        if let Keyword::Set(env) = set {
            assert_eq!(env.derived_store_paths(), [STORE_PATH]);
        }
    }
}
