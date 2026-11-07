use std::{
    env,
    process::{
        Command,
        Output,
    },
};

use super::common::{
    Sandbox,
    stderr,
    stdout,
};

fn shell(sb: &Sandbox, script: &str) -> Output {
    Command::new("bash")
        .args(["--noprofile", "--norc", "-eu", "-c", script])
        .current_dir(&sb.root)
        .env_clear()
        .env("PATH", env::var_os("PATH").unwrap_or_default())
        .env("HOME", &sb.state)
        .env("XDG_STATE_HOME", &sb.state)
        .env("CADE_BIN", env!("CARGO_BIN_EXE_cade"))
        .output()
        .unwrap()
}

#[test]
fn entering_an_unapproved_child_keeps_the_approved_parent_active() {
    let sb = Sandbox::new();
    sb.write(
        ".cade",
        "A=1\nhook load printf 'loaded\\n'\nhook unload printf 'unloaded\\n'\n",
    );
    sb.dir("sub");
    sb.write("sub/.cade", "B=2\n");
    sb.allow(&sb.root);
    let out = shell(
        &sb,
        r#"
eval "$("$CADE_BIN" hook bash)"
_cade_hook
cd sub
_cade_hook
printf 'parent=%s:%s\n' "$A" "${B-unset}"
"#,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "loaded\nparent=1:unset\n");
}
