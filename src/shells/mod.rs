mod elvish;
mod fish;
mod json;
mod murex;
mod nushell;
mod posix;
mod quote;

use std::{
    fmt,
    str::FromStr,
};

use elvish::Elvish;
use fish::Fish;
use json::Json;
use murex::Murex;
use nushell::Nushell;
use posix::{
    Bash,
    Zsh,
};

pub trait ShellOutput {
    fn set_env(&self, key: &str, value: &str) -> String;
    fn unset_env(&self, key: &str) -> String;
    fn emit_hook(&self, command: &str) -> String;
    fn hook_init(&self, cade_exe: &str, cade_args: &[String]) -> String;
}

pub fn is_valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {},
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

#[derive(Debug, Clone, Copy)]
pub enum ShellName {
    Fish,
    Bash,
    Zsh,
    Nushell,
    Json,
    Elvish,
    Murex,
}

impl fmt::Display for ShellName {
    #[expect(
        clippy::renamed_function_params,
        reason = "Display names its formatter parameter f"
    )]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Fish => write!(formatter, "fish"),
            Self::Bash => write!(formatter, "bash"),
            Self::Zsh => write!(formatter, "zsh"),
            Self::Nushell => write!(formatter, "nushell"),
            Self::Json => write!(formatter, "json"),
            Self::Elvish => write!(formatter, "elvish"),
            Self::Murex => write!(formatter, "murex"),
        }
    }
}

impl FromStr for ShellName {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.to_lowercase().as_str() {
            "fish" => Ok(Self::Fish),
            "bash" => Ok(Self::Bash),
            "zsh" => Ok(Self::Zsh),
            "nushell" | "nu" => Ok(Self::Nushell),
            "json" => Ok(Self::Json),
            "elvish" => Ok(Self::Elvish),
            "murex" => Ok(Self::Murex),
            _ => Err(format!("unknown shell: {text}")),
        }
    }
}

impl ShellName {
    pub fn get_output(self) -> Box<dyn ShellOutput> {
        match self {
            Self::Fish => Box::new(Fish),
            Self::Bash => Box::new(Bash),
            Self::Zsh => Box::new(Zsh),
            Self::Nushell => Box::new(Nushell),
            Self::Json => Box::new(Json),
            Self::Elvish => Box::new(Elvish),
            Self::Murex => Box::new(Murex),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOSTILE: &str = r#"$(touch /tmp/pwned)`id`;rm -rf ~ "quote' end"#;

    #[test]
    fn valid_keys() {
        assert!(is_valid_key("PATH"));
        assert!(is_valid_key("_x9"));
        assert!(is_valid_key("A_B_C"));
        assert!(!is_valid_key(""));
        assert!(!is_valid_key("9bad"));
        assert!(!is_valid_key("has space"));
        assert!(!is_valid_key("x;rm -rf"));
        assert!(!is_valid_key("a=b"));
        assert!(!is_valid_key("a$b"));
    }

    #[test]
    fn bash_value_is_single_quoted_and_inert() {
        let out = Bash.set_env("EVIL", HOSTILE);
        assert!(out.starts_with("export EVIL='"));
        assert!(out.ends_with("';"));
        let body = out
            .strip_prefix("export EVIL=")
            .unwrap()
            .strip_suffix(';')
            .unwrap();
        let inner = body.strip_prefix('\'').unwrap().strip_suffix('\'').unwrap();
        let decoded = inner.replace("'\\''", "'");
        assert_eq!(decoded, HOSTILE);
    }

    #[test]
    fn bash_rejects_hostile_keys() {
        assert_eq!(Bash.set_env("x;rm -rf ~", "v"), "");
        assert_eq!(Bash.unset_env("a b"), "");
    }

    #[test]
    fn fish_escapes_quote_and_backslash() {
        let out = Fish.set_env("X", r"a'b\c");
        assert_eq!(out, r"set -gx X 'a\'b\\c';");
    }

    #[test]
    fn elvish_doubles_quotes() {
        assert_eq!(Elvish.set_env("X", "a'b"), "set-env X 'a''b';");
    }

    #[test]
    fn murex_strips_single_quotes_to_stay_inert() {
        let out = Murex.set_env("X", "pa'ss");
        assert_eq!(out, "export X='pass'\n");
    }

    #[test]
    fn nushell_emits_json_data_not_code() {
        let out = Nushell.set_env("X", r#"$(id)"x"#);
        let parsed: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(parsed["s"]["X"], "$(id)\"x");
    }

    #[test]
    fn nushell_emits_path_as_a_list() {
        let out = Nushell.set_env("PATH", "/one:/two");
        let parsed: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(parsed["s"]["PATH"], serde_json::json!(["/one", "/two"]));
    }
}
