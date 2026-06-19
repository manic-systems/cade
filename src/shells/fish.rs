use super::{ShellOutput, is_valid_key, quote::fish_command};

pub struct Fish;

impl ShellOutput for Fish {
    fn set_env(&self, key: &str, value: &str) -> String {
        if !is_valid_key(key) {
            return String::new();
        }
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
    set -l __cade_status $status
    __CADE__ --owner-pid $fish_pid reload --shell fish | source
    return $__cade_status
end
"#
        .replace("__CADE__", &fish_command(cade_exe, cade_args))
    }
}
