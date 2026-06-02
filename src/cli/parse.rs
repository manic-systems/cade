use crate::types::*;
use std::{fmt::Display, str::FromStr};

#[derive(Debug)]
pub enum ParseError {
    InvalidKeyword,
    UnknownLoadable,
    TooManyOptions,
    TooFewOptions,
    InvalidQuoting,
    EmptyLine,
}

impl Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidKeyword => f.write_str("invalid keyword"),
            Self::UnknownLoadable => f.write_str("unknown loadable"),
            Self::TooManyOptions => f.write_str("too many options"),
            Self::TooFewOptions => f.write_str("too few options"),
            Self::InvalidQuoting => f.write_str("unbalanced quotes"),
            Self::EmptyLine => f.write_str("empty line"),
        }
    }
}

// an all-caps env var identifier like `FOO_BAR`, used to tell a bare
// assignment apart from the lowercase directive keywords
fn is_assignment_key(k: &str) -> bool {
    let mut chars = k.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase() || c == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

impl FromStr for Keyword {
    type Err = ParseError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return Err(ParseError::EmptyLine);
        }

        use crate::types::Keyword::*;

        // a bare `KEY=value` (or `KEY:=value`) assignment, recognised only when
        // the key is an all-caps identifier so it can't shadow a keyword
        if let Some((lhs, _)) = trimmed.split_once('=') {
            let key = lhs.strip_suffix(':').unwrap_or(lhs).trim_end();
            if is_assignment_key(key) {
                return EnvSet::from_envs(trimmed)
                    .map(Set)
                    .map_err(|_| ParseError::InvalidKeyword);
            }
        }

        let mut words = trimmed.split_whitespace();
        // slice off the original first word rather than the lowercased copy
        // since lowercasing can change byte boundaries
        let first = words.next().unwrap();
        let keyword = first.to_lowercase();
        let rest_raw = trimmed[first.len()..].trim_start();

        let res = match keyword.as_str() {
            "pure" => Pure,
            "disinherit" => Disinherit,
            "call" => {
                // split respecting shell quoting
                let target = shlex::split(rest_raw).ok_or(ParseError::InvalidQuoting)?;
                if target.is_empty() {
                    return Err(ParseError::TooFewOptions);
                }
                Call(target)
            }
            "load" => {
                let rest: Vec<&str> = rest_raw.split_whitespace().collect();
                if rest.len() > 2 {
                    return Err(ParseError::TooManyOptions);
                }
                match rest.first().map(|s| s.to_lowercase()).as_deref() {
                    None => Load(Loadable::Default),
                    Some("shell") => Load(Loadable::Shell(rest.get(1).unwrap_or(&"").to_string())),
                    Some("flake") => Load(Loadable::Flake(rest.get(1).unwrap_or(&"").to_string())),
                    Some("env") => Load(Loadable::Env(rest.get(1).unwrap_or(&"").to_string())),
                    Some("envrc") => Load(Loadable::Envrc(rest.get(1).unwrap_or(&"").to_string())),
                    Some(_) => return Err(ParseError::UnknownLoadable),
                }
            }
            "hook" => {
                use crate::types::HookType::*;
                // first token is the lifecycle phase, rest is the command
                let (phase, command) = match rest_raw.split_once(char::is_whitespace) {
                    Some((p, c)) => (p, c.trim_start()),
                    None => (rest_raw, ""),
                };
                let (kind, content) = match phase.to_lowercase().as_str() {
                    "preload" => (LoadPre, command),
                    "load" => (LoadPost, command),
                    "preunload" => (UnloadPre, command),
                    "unload" => (UnloadPost, command),
                    // no phase given, treat the whole line as a post-load command
                    _ => (LoadPost, rest_raw),
                };
                if content.is_empty() {
                    return Err(ParseError::TooFewOptions);
                }
                Hook(crate::types::InnerHook {
                    kind,
                    content: content.to_string(),
                })
            }
            "clear" => {
                let vars: Vec<String> = rest_raw.split_whitespace().map(|s| s.to_owned()).collect();
                if vars.is_empty() {
                    return Err(ParseError::TooFewOptions);
                }
                Clear(vars)
            }
            "watch" => {
                let files = shlex::split(rest_raw).ok_or(ParseError::InvalidQuoting)?;
                if files.is_empty() {
                    return Err(ParseError::TooFewOptions);
                }
                Watch(files)
            }
            "concat" => {
                let vars: Vec<String> = rest_raw.split_whitespace().map(|s| s.to_owned()).collect();
                if vars.is_empty() {
                    return Err(ParseError::TooFewOptions);
                }
                Concat(vars)
            }
            _ => {
                crate::verbosity::log(
                    crate::verbosity::Verbosity::Normal,
                    format_args!("found invalid command: {keyword}"),
                );
                return Err(ParseError::InvalidKeyword);
            }
        };
        Ok(res)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_uppercase_assignment_parses() {
        match "FOO=bar".parse::<Keyword>().unwrap() {
            Keyword::Set(env) => assert_eq!(env.vars["FOO"], vec!["bar"]),
            other => panic!("expected Set, got {other:?}"),
        }
    }

    #[test]
    fn hard_replace_assignment_is_marked() {
        match "PATH:=/x".parse::<Keyword>().unwrap() {
            Keyword::Set(env) => {
                assert_eq!(env.vars["PATH"], vec!["/x"]);
                assert!(env.hard.contains("PATH"));
            }
            other => panic!("expected Set, got {other:?}"),
        }
    }

    #[test]
    fn lowercase_key_is_not_an_assignment() {
        // not all-caps, so it falls through to keyword lookup and is rejected
        assert!(matches!(
            "foo=bar".parse::<Keyword>(),
            Err(ParseError::InvalidKeyword)
        ));
    }

    #[test]
    fn multibyte_first_word_does_not_panic() {
        // `İ` (U+0130) lowercases to two bytes, potentially breaking slice indexing
        assert!(matches!(
            "İ foo".parse::<Keyword>(),
            Err(ParseError::InvalidKeyword)
        ));
    }

    #[test]
    fn bare_disinherit_parses() {
        assert!(matches!(
            "disinherit".parse::<Keyword>(),
            Ok(Keyword::Disinherit)
        ));
    }

    #[test]
    fn keyword_with_equals_in_args_stays_a_keyword() {
        // the `=` belongs to the hook command, not a bare assignment
        assert!(matches!(
            "hook load export X=1".parse::<Keyword>(),
            Ok(Keyword::Hook(_))
        ));
    }
}
