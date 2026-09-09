use std::{
    borrow::ToOwned,
    error::Error,
    fmt::{
        Display,
        Formatter,
        Result as FmtResult,
    },
    str::FromStr,
};

use crate::{
    env::set::EnvSet,
    types::{
        hook::{
            HookType,
            InnerHook,
        },
        keyword::{
            Keyword,
            Loadable,
        },
    },
};

#[derive(Debug)]
pub enum ParseError {
    InvalidKeyword,
    InvalidAssignment(Box<dyn Error + Send + Sync>),
    UnknownLoadable,
    TooManyOptions,
    TooFewOptions,
    EmptyLine,
}

impl Display for ParseError {
    #[expect(
        clippy::renamed_function_params,
        reason = "Display names its formatter parameter f"
    )]
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match *self {
            Self::InvalidKeyword => formatter.write_str("invalid keyword"),
            Self::InvalidAssignment(ref error) => {
                write!(formatter, "invalid assignment, {error}")
            },
            Self::UnknownLoadable => formatter.write_str("unknown loadable"),
            Self::TooManyOptions => formatter.write_str("too many options"),
            Self::TooFewOptions => formatter.write_str("too few options"),
            Self::EmptyLine => formatter.write_str("empty line"),
        }
    }
}

impl Error for ParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match *self {
            Self::InvalidAssignment(ref error) => Some(error.as_ref()),
            Self::InvalidKeyword
            | Self::UnknownLoadable
            | Self::TooManyOptions
            | Self::TooFewOptions
            | Self::EmptyLine => None,
        }
    }
}

fn is_assignment_key(candidate: &str) -> bool {
    let mut chars = candidate.chars();
    matches!(chars.next(), Some(head) if head.is_ascii_uppercase() || head == '_')
        && chars.all(|item| item.is_ascii_uppercase() || item.is_ascii_digit() || item == '_')
}

impl FromStr for Keyword {
    type Err = ParseError;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
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

        let trimmed = input.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return Err(ParseError::EmptyLine);
        }

        if let Some((lhs, _)) = trimmed.split_once('=') {
            let key = lhs.strip_suffix(':').unwrap_or(lhs).trim_end();
            if is_assignment_key(key) {
                return EnvSet::from_envs(trimmed)
                    .map(Set)
                    .map_err(|error| ParseError::InvalidAssignment(error.into()));
            }
        }

        let mut words = trimmed.split_whitespace();
        let first = words.next().unwrap();
        let keyword = first.to_lowercase();
        let rest_raw = trimmed.strip_prefix(first).unwrap_or_default().trim_start();

        let res = match keyword.as_str() {
            "pure" => Pure,
            "disinherit" => Disinherit,
            "call" => {
                if rest_raw.is_empty() {
                    return Err(ParseError::TooFewOptions);
                }
                Call(rest_raw.to_owned())
            },
            "load" => {
                let rest: Vec<&str> = rest_raw.split_whitespace().collect();
                if rest.len() > 2 {
                    return Err(ParseError::TooManyOptions);
                }
                match rest.first().map(|word| word.to_lowercase()).as_deref() {
                    None => Load(Loadable::Default),
                    Some("shell") => Load(Loadable::Shell(rest.get(1).unwrap_or(&"").to_string())),
                    Some("flake") => Load(Loadable::Flake(rest.get(1).unwrap_or(&"").to_string())),
                    Some("env") => Load(Loadable::Env(rest.get(1).unwrap_or(&"").to_string())),
                    Some("envrc") => Load(Loadable::Envrc(rest.get(1).unwrap_or(&"").to_string())),
                    Some(_) => return Err(ParseError::UnknownLoadable),
                }
            },
            "hook" => {
                use HookType::{
                    LoadPost,
                    LoadPre,
                    UnloadPost,
                    UnloadPre,
                };
                let (phase, command) = match rest_raw.split_once(char::is_whitespace) {
                    Some((phase_part, tail)) => (phase_part, tail.trim_start()),
                    None => (rest_raw, ""),
                };
                let (kind, content) = match phase.to_lowercase().as_str() {
                    "preload" => (LoadPre, command),
                    "load" => (LoadPost, command),
                    "preunload" => (UnloadPre, command),
                    "unload" => (UnloadPost, command),
                    _ => (LoadPost, rest_raw),
                };
                if content.is_empty() {
                    return Err(ParseError::TooFewOptions);
                }
                Hook(InnerHook {
                    kind,
                    content: content.to_owned(),
                })
            },
            "clear" => {
                let vars: Vec<String> =
                    rest_raw.split_whitespace().map(ToOwned::to_owned).collect();
                if vars.is_empty() {
                    return Err(ParseError::TooFewOptions);
                }
                Clear(vars)
            },
            "watch" => {
                if rest_raw.is_empty() {
                    return Err(ParseError::TooFewOptions);
                }
                Watch(rest_raw.to_owned())
            },
            "concat" => {
                let vars: Vec<String> =
                    rest_raw.split_whitespace().map(ToOwned::to_owned).collect();
                if vars.is_empty() {
                    return Err(ParseError::TooFewOptions);
                }
                Concat(vars)
            },
            _ => {
                return Err(ParseError::InvalidKeyword);
            },
        };
        Ok(res)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::set::EnvSet;

    fn env_values(env: &EnvSet, key: &str) -> Vec<String> {
        serde_json::to_value(env).unwrap()["vars"][key]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect()
    }

    fn env_hard_replace_contains(env: &EnvSet, key: &str) -> bool {
        serde_json::to_value(env).unwrap()["hard"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == key)
    }

    #[test]
    fn bare_uppercase_assignment_parses() {
        let parsed = "FOO=bar".parse::<Keyword>().unwrap();
        assert!(matches!(parsed, Keyword::Set(_)));
        if let Keyword::Set(env) = parsed {
            assert_eq!(env_values(&env, "FOO"), vec!["bar"]);
        }
    }

    #[test]
    fn hard_replace_assignment_is_recorded() {
        let parsed = "PATH:=/x".parse::<Keyword>().unwrap();
        assert!(matches!(parsed, Keyword::Set(_)));
        if let Keyword::Set(env) = parsed {
            assert_eq!(env_values(&env, "PATH"), vec!["/x"]);
            assert!(env_hard_replace_contains(&env, "PATH"));
        }
    }

    #[test]
    fn lowercase_key_is_not_an_assignment() {
        assert!(matches!(
            "foo=bar".parse::<Keyword>(),
            Err(ParseError::InvalidKeyword)
        ));
    }

    #[test]
    fn multibyte_first_word_does_not_panic() {
        assert!(matches!(
            "\u{130} foo".parse::<Keyword>(),
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
        assert!(matches!(
            "hook load export X=1".parse::<Keyword>(),
            Ok(Keyword::Hook(_))
        ));
    }
}
