//! bash-style `${...}` expansion for `.cade` directive arguments. only the
//! braced form is recognized; `${VAR}`, `${VAR:-w}`, `${VAR-w}`, `${VAR:+w}`,
//! and `${VAR+w}` behave as in bash. `\$` is a literal `$`; a bare `$VAR` is
//! left verbatim.

use crate::{
    env::EnvSet,
    types::{Keyword, Loadable},
};

type Lookup<'a> = &'a dyn Fn(&str) -> Option<String>;

pub(crate) fn expand_keyword(kw: &mut Keyword) {
    expand_keyword_with(kw, &|k| std::env::var(k).ok());
}

fn expand_keyword_with(kw: &mut Keyword, lookup: Lookup<'_>) {
    use Keyword::*;
    match kw {
        // survive later shlex splitting
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
    env.map_joined_values(|value| expand_plain(value, lookup));
}

fn expand_plain(input: &str, lookup: Lookup<'_>) -> String {
    expand_with(input, lookup, &|v| v)
}

fn expand_shell_args(input: &str, lookup: Lookup<'_>) -> String {
    expand_with(input, lookup, &|v| {
        shlex::try_quote(&v).map(|c| c.into_owned()).unwrap_or(v)
    })
}

fn expand_with(input: &str, lookup: Lookup<'_>, on_value: &dyn Fn(String) -> String) -> String {
    let bytes = input.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            // consume `\$`
            b'\\' if bytes.get(i + 1) == Some(&b'$') => {
                out.push('$');
                i += 2;
            }
            b'$' if bytes.get(i + 1) == Some(&b'{') => match find_close(input, i) {
                Some((inner, end)) => {
                    out.push_str(&on_value(expand_ref(inner, lookup)));
                    i = end;
                }
                // keep unterminated `${`
                None => {
                    out.push_str("${");
                    i += 2;
                }
            },
            _ => {
                let ch = input[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

fn find_close(s: &str, start: usize) -> Option<(&str, usize)> {
    let bytes = s.as_bytes();
    let mut depth = 1usize;
    let mut j = start + 2;
    while j < bytes.len() {
        if bytes[j] == b'$' && bytes.get(j + 1) == Some(&b'{') {
            depth += 1;
            j += 2;
        } else if bytes[j] == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some((&s[start + 2..j], j + 1));
            }
            j += 1;
        } else {
            j += 1;
        }
    }
    None
}

fn is_name_byte(b: u8, first: bool) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || (!first && b.is_ascii_digit())
}

fn expand_ref(inner: &str, lookup: Lookup<'_>) -> String {
    let bytes = inner.as_bytes();
    let mut n = 0;
    while n < bytes.len() && is_name_byte(bytes[n], n == 0) {
        n += 1;
    }
    if n == 0 {
        return format!("${{{inner}}}");
    }
    let name = &inner[..n];
    let rest = &inner[n..];
    let val = lookup(name);

    if rest.is_empty() {
        return val.unwrap_or_default();
    }

    let unset_or_empty = val.as_deref().is_none_or(str::is_empty);
    if let Some(word) = rest.strip_prefix(":-") {
        return if unset_or_empty {
            expand_plain(word, lookup)
        } else {
            val.unwrap_or_default()
        };
    }
    if let Some(word) = rest.strip_prefix(":+") {
        return if unset_or_empty {
            String::new()
        } else {
            expand_plain(word, lookup)
        };
    }
    if let Some(word) = rest.strip_prefix('-') {
        return val.unwrap_or_else(|| expand_plain(word, lookup));
    }
    if let Some(word) = rest.strip_prefix('+') {
        return if val.is_some() {
            expand_plain(word, lookup)
        } else {
            String::new()
        };
    }

    format!("${{{inner}}}")
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

    fn exp(input: &str, pairs: &[(&str, &str)]) -> String {
        expand_plain(input, &lookup_from(pairs))
    }

    // resolve a `call`/`watch` raw string the way the load path does: expand
    // (shell-quoting values), then shlex-tokenize
    fn args(input: &str, pairs: &[(&str, &str)]) -> Vec<String> {
        let expanded = expand_shell_args(input, &lookup_from(pairs));
        shlex::split(&expanded).expect("balanced quotes")
    }

    #[test]
    fn substitutes_a_bare_reference() {
        assert_eq!(exp("hi ${NAME}!", &[("NAME", "bob")]), "hi bob!");
    }

    #[test]
    fn unset_bare_reference_is_empty() {
        assert_eq!(exp("[${NOPE}]", &[]), "[]");
    }

    #[test]
    fn colon_dash_default_is_optional() {
        assert_eq!(exp("${VAR:-}", &[("VAR", "x")]), "x");
        assert_eq!(exp("${VAR:-}", &[]), "");
    }

    #[test]
    fn colon_dash_uses_default_when_unset_or_empty() {
        assert_eq!(exp("${V:-fallback}", &[]), "fallback");
        assert_eq!(exp("${V:-fallback}", &[("V", "")]), "fallback");
        assert_eq!(exp("${V:-fallback}", &[("V", "set")]), "set");
    }

    #[test]
    fn dash_uses_default_only_when_unset() {
        assert_eq!(exp("${V-fallback}", &[]), "fallback");
        assert_eq!(exp("${V-fallback}", &[("V", "")]), "");
        assert_eq!(exp("${V-fallback}", &[("V", "set")]), "set");
    }

    #[test]
    fn colon_plus_alternate_requires_non_empty() {
        assert_eq!(exp("${V:+yes}", &[("V", "x")]), "yes");
        assert_eq!(exp("${V:+yes}", &[("V", "")]), "");
        assert_eq!(exp("${V:+yes}", &[]), "");
    }

    #[test]
    fn plus_alternate_requires_only_set() {
        assert_eq!(exp("${V+yes}", &[("V", "x")]), "yes");
        assert_eq!(exp("${V+yes}", &[("V", "")]), "yes");
        assert_eq!(exp("${V+yes}", &[]), "");
    }

    #[test]
    fn default_word_is_expanded_recursively() {
        assert_eq!(exp("${A:-${B}}", &[("B", "deep")]), "deep");
        assert_eq!(exp("${A:-${B:-lit}}", &[]), "lit");
    }

    #[test]
    fn multiple_refs_and_surrounding_text() {
        assert_eq!(exp("${A}/${B}/end", &[("A", "x"), ("B", "y")]), "x/y/end");
    }

    #[test]
    fn expanded_value_is_not_word_split() {
        assert_eq!(exp("${V}", &[("V", "a b c")]), "a b c");
    }

    #[test]
    fn unterminated_brace_is_literal() {
        assert_eq!(exp("${A", &[("A", "x")]), "${A");
        assert_eq!(exp("pre ${A", &[("A", "x")]), "pre ${A");
    }

    #[test]
    fn unknown_operator_and_empty_name_are_left_verbatim() {
        assert_eq!(exp("${V:?nope}", &[("V", "x")]), "${V:?nope}");
        assert_eq!(exp("${V:=x}", &[("V", "y")]), "${V:=x}");
        assert_eq!(exp("${}", &[]), "${}");
    }

    #[test]
    fn bare_dollar_without_brace_is_left_alone() {
        assert_eq!(exp("$VAR and $$", &[("VAR", "x")]), "$VAR and $$");
    }

    #[test]
    fn backslash_dollar_is_a_literal_dollar() {
        // the escape suppresses expansion and drops the backslash
        assert_eq!(exp("\\${X}", &[("X", "v")]), "${X}");
        assert_eq!(exp("\\$X", &[("X", "v")]), "$X");
        // mixed: one escaped, one expanded
        assert_eq!(exp("\\${X}=${X}", &[("X", "v")]), "${X}=v");
        // an escaped default inside an expansion stays literal
        assert_eq!(exp("${A:-\\${B}}", &[("B", "v")]), "${B}");
    }

    #[test]
    fn lone_backslash_is_preserved() {
        assert_eq!(exp("a\\b", &[]), "a\\b");
    }

    #[test]
    fn call_arg_single_backslash_escapes_uniformly() {
        // the whole point of the refactor: one backslash escapes in call args,
        // same as inline/load, despite the later shlex split
        assert_eq!(
            args("echo \\${VAR}", &[("VAR", "v")]),
            vec!["echo", "${VAR}"]
        );
        assert_eq!(args("echo ${VAR}", &[("VAR", "v")]), vec!["echo", "v"]);
    }

    #[test]
    fn call_arg_value_is_one_token_even_with_spaces_or_quotes() {
        // shell-quoting the substituted value keeps it a single argument
        assert_eq!(args("run ${A}", &[("A", "a b c")]), vec!["run", "a b c"]);
        assert_eq!(args("run ${A}", &[("A", "a'b")]), vec!["run", "a'b"]);
        // user-authored quoting in the template still groups normally
        assert_eq!(
            args("run \"x y\" ${A}", &[("A", "z")]),
            vec!["run", "x y", "z"]
        );
    }

    #[test]
    fn walks_call_and_load_but_not_hook() {
        let lookup = lookup_from(&[("TOKEN", "secret"), ("DIR", "/srv")]);
        let mut call = Keyword::Call("tool --t=${TOKEN}".into());
        expand_keyword_with(&mut call, &lookup);
        match call {
            // the stored string is shell-ready; the load path shlex-splits it
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
            Keyword::Set(env) => assert_eq!(env.values("MODE").unwrap(), ["dev"]),
            other => panic!("expected Set, got {other:?}"),
        }
    }

    #[test]
    fn walks_inline_assignment_value_keeping_colon_lists() {
        let lookup = lookup_from(&[("EXTRA", "/a:/b")]);
        let mut set = "MYPATH=${EXTRA}:/c".parse::<Keyword>().unwrap();
        expand_keyword_with(&mut set, &lookup);
        match set {
            Keyword::Set(env) => assert_eq!(env.values("MYPATH").unwrap(), ["/a", "/b", "/c"]),
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
            Keyword::Set(env) => assert_eq!(env.store_paths(), [STORE_PATH]),
            other => panic!("expected Set, got {other:?}"),
        }
    }
}
