mod elvish;
mod fish;
mod json;
mod murex;
mod nushell;
mod posix;
mod quote;

pub use elvish::Elvish;
pub use fish::Fish;
pub use json::Json;
pub use murex::Murex;
pub use nushell::Nushell;
pub use posix::{Bash, Zsh};

use std::{fmt, str::FromStr};

pub trait ShellOutput {
    fn set_env(&self, key: &str, value: &str) -> String;
    fn unset_env(&self, key: &str) -> String;
    fn emit_hook(&self, command: &str) -> String;
    fn hook_init(&self, cade_exe: &str, cade_args: &[String]) -> String;
}

pub fn is_valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShellName::Fish => write!(f, "fish"),
            ShellName::Bash => write!(f, "bash"),
            ShellName::Zsh => write!(f, "zsh"),
            ShellName::Nushell => write!(f, "nushell"),
            ShellName::Json => write!(f, "json"),
            ShellName::Elvish => write!(f, "elvish"),
            ShellName::Murex => write!(f, "murex"),
        }
    }
}

impl FromStr for ShellName {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "fish" => Ok(ShellName::Fish),
            "bash" => Ok(ShellName::Bash),
            "zsh" => Ok(ShellName::Zsh),
            "nushell" | "nu" => Ok(ShellName::Nushell),
            "json" => Ok(ShellName::Json),
            "elvish" => Ok(ShellName::Elvish),
            "murex" => Ok(ShellName::Murex),
            _ => Err(format!("unknown shell: {s}")),
        }
    }
}

impl ShellName {
    pub fn get_output(&self) -> Box<dyn ShellOutput> {
        match self {
            ShellName::Fish => Box::new(Fish),
            ShellName::Bash => Box::new(Bash),
            ShellName::Zsh => Box::new(Zsh),
            ShellName::Nushell => Box::new(Nushell),
            ShellName::Json => Box::new(Json),
            ShellName::Elvish => Box::new(Elvish),
            ShellName::Murex => Box::new(Murex),
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
        let inner = &body[1..body.len() - 1];
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
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(v["s"]["X"], "$(id)\"x");
    }

    #[test]
    fn nushell_emits_path_as_a_list() {
        let out = Nushell.set_env("PATH", "/one:/two");
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(v["s"]["PATH"], serde_json::json!(["/one", "/two"]));
    }
}
