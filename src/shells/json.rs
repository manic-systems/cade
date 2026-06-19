use super::{ShellOutput, is_valid_key};

pub struct Json;

pub(super) fn set_directive(key: &str, value: &str) -> String {
    let mut rec = serde_json::Map::new();
    rec.insert(key.to_string(), env_value(key, value));
    format!("{}\n", serde_json::json!({ "s": rec }))
}

pub(super) fn unset_directive(key: &str) -> String {
    format!("{}\n", serde_json::json!({ "u": key }))
}

pub(super) fn hook_directive(command: &str) -> String {
    format!("{}\n", serde_json::json!({ "h": command }))
}

fn env_value(key: &str, value: &str) -> serde_json::Value {
    if key == "PATH" {
        return serde_json::Value::Array(
            std::env::split_paths(value)
                .map(|path| serde_json::Value::from(path.to_string_lossy().into_owned()))
                .collect(),
        );
    }

    serde_json::Value::from(value)
}

impl ShellOutput for Json {
    fn set_env(&self, key: &str, value: &str) -> String {
        if !is_valid_key(key) {
            return String::new();
        }
        set_directive(key, value)
    }

    fn unset_env(&self, key: &str) -> String {
        if !is_valid_key(key) {
            return String::new();
        }
        unset_directive(key)
    }

    fn emit_hook(&self, command: &str) -> String {
        hook_directive(command)
    }

    fn hook_init(&self, _cade_exe: &str, _cade_args: &[String]) -> String {
        String::new()
    }
}
