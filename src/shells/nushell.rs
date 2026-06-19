use super::{
    ShellOutput, is_valid_key,
    json::{hook_directive, set_directive, unset_directive},
};

pub struct Nushell;

impl ShellOutput for Nushell {
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

    fn hook_init(&self, cade_exe: &str, cade_args: &[String]) -> String {
        let cade = serde_json::to_string(cade_exe).unwrap_or_else(|_| "\"cade\"".to_string());
        let cade_args = serde_json::to_string(cade_args).unwrap_or_else(|_| "[]".to_string());
        r#"let cade = __CADE__
let cade_args = __CADE_ARGS__
let nu_exe = (try { which nu | get path.0 } catch { "nu" })

$env.config.hooks.pre_prompt = (
    ($env.config.hooks?.pre_prompt? | default [])
    | append {||
        let __cade_last_exit = ($env.LAST_EXIT_CODE? | default 0)
        for line in (^$cade ...$cade_args --owner-pid $nu.pid reload --shell nushell | lines) {
            if ($line | str trim | is-empty) { continue }
            let m = ($line | from json)
            if "s" in $m { load-env $m.s }
            if "u" in $m { hide-env --ignore-errors $m.u }
            if "h" in $m {
                let prog = ("let __pre = $env\n" + $m.h + "\nlet __post = $env\nlet __set = ($__post | transpose k v | where {|r| ($r.v | describe) == \"string\" and $r.k not-in [PWD OLDPWD] and (($__pre | get --optional $r.k) != $r.v)} | reduce -f {} {|r, a| $a | upsert $r.k $r.v}); {set: $__set, unset: ($__pre | columns | where {|k| $k not-in ($__post | columns)})} | to json | print --stderr")
                let d = (^$nu_exe --no-config-file --commands $prog err>| from json)
                load-env $d.set
                for k in $d.unset { hide-env --ignore-errors $k }
            }
        }
        $env.LAST_EXIT_CODE = $__cade_last_exit
    }
)
"#
        .replace("__CADE__", &cade)
        .replace("__CADE_ARGS__", &cade_args)
    }
}
