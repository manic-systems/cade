use crate::verbosity::{self, Verbosity};
use std::fmt;
use std::str::FromStr;

pub trait ShellOutput {
    fn set_env(&self, key: &str, value: &str) -> String;
    fn unset_env(&self, key: &str) -> String;
    fn emit_hook(&self, command: &str) -> String;
    fn hook_init(&self, cade_exe: &str, cade_args: &[String]) -> String;
}

/// A valid env var identifier. Prevents breakout
pub fn is_valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// wrap single quote in '...', encoding embedded quotes with backslash
fn posix_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

fn fish_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn posix_command(cade_exe: &str, cade_args: &[String]) -> String {
    std::iter::once(cade_exe)
        .chain(cade_args.iter().map(String::as_str))
        .map(posix_single_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

fn fish_command(cade_exe: &str, cade_args: &[String]) -> String {
    std::iter::once(cade_exe)
        .chain(cade_args.iter().map(String::as_str))
        .map(fish_single_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, Copy)]
pub enum ShellName {
    Fish,
    Bash,
    Zsh,
    Nushell,
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
            ShellName::Elvish => Box::new(Elvish),
            ShellName::Murex => Box::new(Murex),
        }
    }
}

// --- Fish ---

pub struct Fish;

impl ShellOutput for Fish {
    fn set_env(&self, key: &str, value: &str) -> String {
        if !is_valid_key(key) {
            return String::new();
        }
        // fish single-quotes escape only `\'` and `\\`; everything else is literal
        let val = value.replace('\\', "\\\\").replace('\'', "\\'");
        format!("set -gx {key} '{val}';")
    }
    fn unset_env(&self, key: &str) -> String {
        if !is_valid_key(key) {
            return String::new();
        }
        format!("set -e {key};")
    }
    fn emit_hook(&self, command: &str) -> String {
        format!("{command};")
    }
    fn hook_init(&self, cade_exe: &str, cade_args: &[String]) -> String {
        r#"function __cade_hook --on-event fish_prompt
    if test "$PWD" != "$__cade_last_pwd"; or set -q __CADE_LAYERS; or set -q __cade_has_file
        __CADE__ reload --shell fish | source
        set -g __cade_last_pwd $PWD
        if test -f "$PWD/.cade"; or test -f "$PWD/.envrc"
            set -g __cade_has_file 1
        else
            set -e __cade_has_file
        end
    end
end
"#
        .replace("__CADE__", &fish_command(cade_exe, cade_args))
    }
}

// --- Bash ---

pub struct Bash;

impl ShellOutput for Bash {
    fn set_env(&self, key: &str, value: &str) -> String {
        if !is_valid_key(key) {
            return String::new();
        }
        format!("export {key}={val};", val = posix_single_quote(value))
    }
    fn unset_env(&self, key: &str) -> String {
        if !is_valid_key(key) {
            return String::new();
        }
        format!("unset {key};")
    }
    fn emit_hook(&self, command: &str) -> String {
        format!("{command};")
    }
    fn hook_init(&self, cade_exe: &str, cade_args: &[String]) -> String {
        r#"_cade_hook() {
    local previous_exit_status=$?
    if [[ "$PWD" != "$__cade_last_pwd" || -n "${__CADE_LAYERS:-}" || -n "${__cade_has_file:-}" ]]; then
        eval "$(__CADE__ reload --shell bash)"
        __cade_last_pwd="$PWD"
        if [[ -f "$PWD/.cade" || -f "$PWD/.envrc" ]]; then
            __cade_has_file=1
        else
            __cade_has_file=
        fi
    fi
    return $previous_exit_status
}
if [[ ";${PROMPT_COMMAND[*]:-};" != *";_cade_hook;"* ]]; then
    PROMPT_COMMAND="_cade_hook;${PROMPT_COMMAND:-}"
fi
"#
        .replace("__CADE__", &posix_command(cade_exe, cade_args))
    }
}

// --- Zsh ---

pub struct Zsh;

impl ShellOutput for Zsh {
    fn set_env(&self, key: &str, value: &str) -> String {
        if !is_valid_key(key) {
            return String::new();
        }
        format!("export {key}={val};", val = posix_single_quote(value))
    }
    fn unset_env(&self, key: &str) -> String {
        if !is_valid_key(key) {
            return String::new();
        }
        format!("unset {key};")
    }
    fn emit_hook(&self, command: &str) -> String {
        format!("{command};")
    }
    fn hook_init(&self, cade_exe: &str, cade_args: &[String]) -> String {
        r#"_cade_hook() {
    if [[ "$PWD" != "$__cade_last_pwd" || -n "${__CADE_LAYERS:-}" || -n "${__cade_has_file:-}" ]]; then
        eval "$(__CADE__ reload --shell zsh)"
        __cade_last_pwd="$PWD"
        if [[ -f "$PWD/.cade" || -f "$PWD/.envrc" ]]; then
            __cade_has_file=1
        else
            __cade_has_file=
        fi
    fi
}
typeset -ag precmd_functions
if (( ! ${precmd_functions[(I)_cade_hook]} )); then
    precmd_functions=(_cade_hook $precmd_functions)
fi
typeset -ag chpwd_functions
if (( ! ${chpwd_functions[(I)_cade_hook]} )); then
    chpwd_functions=(_cade_hook $chpwd_functions)
fi
"#
        .replace("__CADE__", &posix_command(cade_exe, cade_args))
    }
}

// --- Nushell ---

pub struct Nushell;

impl ShellOutput for Nushell {
    // Nushell can't `source` a per-shell file (source needs a const path) and
    // `nu -c` loses env, so cade emits NDJSON directives that the prompt closure
    // applies in-scope with `load-env`/`hide-env`. No shared file, so no races.
    fn set_env(&self, key: &str, value: &str) -> String {
        if !is_valid_key(key) {
            return String::new();
        }
        let mut rec = serde_json::Map::new();
        rec.insert(key.to_string(), serde_json::Value::from(value));
        format!("{}\n", serde_json::json!({ "s": rec }))
    }
    fn unset_env(&self, key: &str) -> String {
        if !is_valid_key(key) {
            return String::new();
        }
        format!("{}\n", serde_json::json!({ "u": key }))
    }
    fn emit_hook(&self, command: &str) -> String {
        format!("{}\n", serde_json::json!({ "h": command }))
    }
    fn hook_init(&self, cade_exe: &str, cade_args: &[String]) -> String {
        let cade = serde_json::to_string(cade_exe).unwrap_or_else(|_| "\"cade\"".to_string());
        let cade_args = serde_json::to_string(cade_args).unwrap_or_else(|_| "[]".to_string());
        r#"let cade = __CADE__
let cade_args = __CADE_ARGS__
let nu_exe = (try { which nu | get path.0 } catch { "nu" })

$env.config.hooks.pre_prompt = (
    ($env.config.hooks?.pre_prompt? | default [])
    | append {||
        # Nushell sets $env.__cade_has_file to string or bool, handle both. I can't figure out why.
        if ($env.PWD != ($env.__cade_last_pwd? | default "")) or ("__CADE_LAYERS" in $env) or (($env.__cade_has_file? in [true "true"]) | default false) {
            for line in (^$cade ...$cade_args reload --shell nushell | lines) {
                if ($line | str trim | is-empty) { continue }
                let m = ($line | from json)
                if "s" in $m { load-env $m.s }
                if "u" in $m { hide-env --ignore-errors $m.u }
                if "h" in $m {
                    # run hook in a child nu, detect env changes by diffing
                    # pre/post $env in-process. JSON diff goes to stderr via
                    # print --stderr so hook stdout can't contaminate it.
                    let prog = ("let __pre = $env\n" + $m.h + "\nlet __post = $env\nlet __set = ($__post | transpose k v | where {|r| ($r.v | describe) == \"string\" and $r.k not-in [PWD OLDPWD] and (($__pre | get --optional $r.k) != $r.v)} | reduce -f {} {|r, a| $a | upsert $r.k $r.v}); {set: $__set, unset: ($__pre | columns | where {|k| $k not-in ($__post | columns)})} | to json | print --stderr")
                    let d = (^$nu_exe --no-config-file --commands $prog err>| from json)
                    load-env $d.set
                    for k in $d.unset { hide-env --ignore-errors $k }
                }
            }
            $env.__cade_last_pwd = $env.PWD
            $env.__cade_has_file = (($"($env.PWD)/.cade" | path exists) or ($"($env.PWD)/.envrc" | path exists))
        }
    }
)
"#
        .replace("__CADE__", &cade)
        .replace("__CADE_ARGS__", &cade_args)
    }
}

// --- Elvish ---

pub struct Elvish;

impl ShellOutput for Elvish {
    fn set_env(&self, key: &str, value: &str) -> String {
        if !is_valid_key(key) {
            return String::new();
        }
        // Elvish single-quoted strings escape an embedded quote by doubling it.
        format!("set-env {key} '{val}';", val = value.replace('\'', "''"))
    }
    fn unset_env(&self, key: &str) -> String {
        if !is_valid_key(key) {
            return String::new();
        }
        format!("unset-env {key};")
    }
    fn emit_hook(&self, command: &str) -> String {
        format!("{command};")
    }
    fn hook_init(&self, cade_exe: &str, cade_args: &[String]) -> String {
        r#"var cade-last-pwd = ''
var cade-has-file = $false
set edit:before-readline = [
    {||
        if (or (not-eq $pwd $cade-last-pwd) (has-env __CADE_LAYERS) $cade-has-file) {
            eval (__CADE_CMD__ reload --shell elvish | slurp)
            set cade-last-pwd = $pwd
            set cade-has-file = (or ?(test -f $pwd/.cade) ?(test -f $pwd/.envrc))
        }
    }
]
"#
        .replace("__CADE_CMD__", &posix_command(cade_exe, cade_args))
    }
}

// --- Murex ---

pub struct Murex;

impl ShellOutput for Murex {
    fn set_env(&self, key: &str, value: &str) -> String {
        if !is_valid_key(key) {
            return String::new();
        }
        // murex single-quotes are fully literal but can't contain a single quote
        if value.contains('\'') {
            verbosity::log(
                Verbosity::Normal,
                format_args!(
                    "cade: warning: murex cannot represent a single quote in ${key}; \
                     stripping it from the value"
                ),
            );
        }
        format!("export {key}='{val}'\n", val = value.replace('\'', ""))
    }
    fn unset_env(&self, key: &str) -> String {
        if !is_valid_key(key) {
            return String::new();
        }
        format!("!export {key}\n")
    }
    fn emit_hook(&self, command: &str) -> String {
        format!("{command}\n")
    }
    fn hook_init(&self, cade_exe: &str, cade_args: &[String]) -> String {
        // murex runs cade on every prompt (no PWD-change fast-path): its
        // conditional syntax made the guard unreliable
        r#"event onPrompt cade=before {
    __CADE__ reload --shell murex -> source
}
"#
        .replace("__CADE__", &posix_command(cade_exe, cade_args))
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
    fn hooks_use_supplied_cade_executable() {
        let exe = "/tmp/cade bin/cade";
        let args = vec!["--config".to_string(), "/tmp/cade config.toml".to_string()];
        assert!(Bash.hook_init(exe, &args).contains(
            "'/tmp/cade bin/cade' '--config' '/tmp/cade config.toml' reload --shell bash"
        ));
        assert!(Zsh.hook_init(exe, &args).contains(
            "'/tmp/cade bin/cade' '--config' '/tmp/cade config.toml' reload --shell zsh"
        ));
        assert!(Fish.hook_init(exe, &args).contains(
            "'/tmp/cade bin/cade' '--config' '/tmp/cade config.toml' reload --shell fish"
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
    fn bash_value_is_single_quoted_and_inert() {
        let out = Bash.set_env("EVIL", HOSTILE);
        // No unescaped `$(`, backtick, or double-quote context that bash expands.
        assert!(out.starts_with("export EVIL='"));
        assert!(out.ends_with("';"));
        // the only way out of a single-quoted string is `'\''`
        let body = out
            .strip_prefix("export EVIL=")
            .unwrap()
            .strip_suffix(';')
            .unwrap();
        // decode `'\''` and strip the wrapping quotes to recover the original
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
        // value stays wrapped in literal single quotes, embedded quote removed
        assert_eq!(out, "export X='pass'\n");
    }

    #[test]
    fn nushell_emits_json_data_not_code() {
        let out = Nushell.set_env("X", r#"$(id)"x"#);
        // a JSON directive parsed with `from json`, never run as nu code
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(v["s"]["X"], "$(id)\"x");
    }
}
