use super::{ShellOutput, is_valid_key, quote::posix_command};
use crate::verbosity::{self, Verbosity};

pub struct Murex;

impl ShellOutput for Murex {
    fn set_env(&self, key: &str, value: &str) -> String {
        if !is_valid_key(key) {
            return String::new();
        }
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
        r#"event onPrompt cade=before {
    __CADE__ reload --shell murex -> source
}
"#
        .replace("__CADE__", &posix_command(cade_exe, cade_args))
    }
}
