use super::{ShellOutput, is_valid_key, quote::posix_command};

pub struct Elvish;

impl ShellOutput for Elvish {
    fn set_env(&self, key: &str, value: &str) -> String {
        if !is_valid_key(key) {
            return String::new();
        }
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
        r#"set edit:before-readline = [
    {||
        eval (__CADE_CMD__ reload --shell elvish | slurp)
    }
]
"#
        .replace("__CADE_CMD__", &posix_command(cade_exe, cade_args))
    }
}
