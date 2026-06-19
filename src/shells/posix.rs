use super::{
    ShellOutput, is_valid_key,
    quote::{posix_command, posix_single_quote},
};

pub struct Bash;
pub struct Zsh;

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
    trap -- '' INT
    eval "$(__CADE__ --owner-pid $$ reload --shell bash)"
    trap - INT
    return $previous_exit_status
}
if [[ ";${PROMPT_COMMAND[*]:-};" != *";_cade_hook;"* ]]; then
    PROMPT_COMMAND="_cade_hook;${PROMPT_COMMAND:-}"
fi
"#
        .replace("__CADE__", &posix_command(cade_exe, cade_args))
    }
}

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
    local previous_exit_status=$?
    trap -- '' INT
    eval "$(__CADE__ --owner-pid $$ reload --shell zsh)"
    trap - INT
    return $previous_exit_status
}
typeset -ag precmd_functions
if (( ! ${precmd_functions[(I)_cade_hook]} )); then
    precmd_functions=(_cade_hook $precmd_functions)
fi
"#
        .replace("__CADE__", &posix_command(cade_exe, cade_args))
    }
}
