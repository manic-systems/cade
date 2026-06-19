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
    fn shell_name_accepts_json() {
        assert!(matches!("json".parse::<ShellName>(), Ok(ShellName::Json)));
        assert_eq!(ShellName::Json.to_string(), "json");
    }

    #[test]
    fn hooks_use_supplied_cade_executable() {
        let exe = "/tmp/cade bin/cade";
        let args = vec!["--config".to_string(), "/tmp/cade config.toml".to_string()];
        assert!(Bash.hook_init(exe, &args).contains(
            "'/tmp/cade bin/cade' '--config' '/tmp/cade config.toml' --owner-pid $$ reload --shell bash"
        ));
        assert!(Zsh.hook_init(exe, &args).contains(
            "'/tmp/cade bin/cade' '--config' '/tmp/cade config.toml' --owner-pid $$ reload --shell zsh"
        ));
        assert!(Fish.hook_init(exe, &args).contains(
            "'/tmp/cade bin/cade' '--config' '/tmp/cade config.toml' --owner-pid $fish_pid reload --shell fish"
        ));
        assert!(
            Nushell
                .hook_init(exe, &args)
                .contains(r#"let cade = "/tmp/cade bin/cade""#)
        );
        assert!(
            Nushell
                .hook_init(exe, &args)
                .contains(r#"let cade_args = ["--config","/tmp/cade config.toml"]"#)
        );
    }

    #[test]
    fn prompt_hooks_reload_without_pwd_change_guard() {
        for hook in [
            Bash.hook_init("cade", &[]),
            Zsh.hook_init("cade", &[]),
            Fish.hook_init("cade", &[]),
            Nushell.hook_init("cade", &[]),
            Elvish.hook_init("cade", &[]),
        ] {
            assert!(hook.contains("reload --shell"), "{hook}");
            assert!(!hook.contains("__cade_last_pwd"), "{hook}");
            assert!(!hook.contains("cade-last-pwd"), "{hook}");
        }
    }

    fn run_in_shell(shell: &str, args: &[&str], script: &str) -> Option<String> {
        let mut full: Vec<&str> = args.to_vec();
        full.push(script);
        let out = std::process::Command::new(shell)
            .args(&full)
            .output()
            .ok()?;
        Some(format!(
            "{}|stderr:{}",
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }

    #[test]
    fn bash_hook_preserves_exit_status() {
        let hook = Bash.hook_init("true", &[]);
        let script = format!("{hook}\n(exit 7)\n_cade_hook\necho $?");
        let Some(out) = run_in_shell("bash", &["-c"], &script) else {
            return;
        };
        assert_eq!(out, "7|stderr:", "bash did not preserve $?: {out}");
    }

    #[test]
    fn fish_hook_preserves_exit_status() {
        let hook = Fish.hook_init("true", &[]);
        let script = format!("{hook}\nfalse\n__cade_hook\necho $status");
        let Some(out) = run_in_shell("fish", &["-c"], &script) else {
            return;
        };
        assert_eq!(out, "1|stderr:", "fish did not preserve $status: {out}");
    }

    #[test]
    fn nushell_hook_preserves_last_exit_code() {
        let hook = Nushell.hook_init("true", &[]);
        let script = format!(
            "{hook}\n$env.LAST_EXIT_CODE = 7\ndo ($env.config.hooks.pre_prompt | last)\nprint $env.LAST_EXIT_CODE"
        );
        let Some(out) = run_in_shell("nu", &["--no-config-file", "-c"], &script) else {
            return;
        };
        assert_eq!(
            out, "7|stderr:",
            "nu did not preserve LAST_EXIT_CODE: {out}"
        );
    }

    #[test]
    fn zsh_hook_registers_only_in_precmd() {
        let zsh = Zsh.hook_init("cade", &[]);
        assert!(zsh.contains("precmd_functions"), "{zsh}");
        assert!(!zsh.contains("chpwd_functions"), "{zsh}");
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

    #[test]
    fn json_shell_uses_nushell_directive_shape() {
        let set: serde_json::Value = serde_json::from_str(Json.set_env("X", "1").trim()).unwrap();
        let unset: serde_json::Value = serde_json::from_str(Json.unset_env("X").trim()).unwrap();
        let hook: serde_json::Value =
            serde_json::from_str(Json.emit_hook("echo ready").trim()).unwrap();

        assert_eq!(set, serde_json::json!({ "s": { "X": "1" } }));
        assert_eq!(unset, serde_json::json!({ "u": "X" }));
        assert_eq!(hook, serde_json::json!({ "h": "echo ready" }));
        assert_eq!(Json.hook_init("cade", &[]), "");
    }
}
