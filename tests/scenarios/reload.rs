use std::{
    env,
    process::{
        Command,
        Output,
    },
};

use common::{
    Sandbox,
    stderr,
    stdout,
};

use super::common;

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
fn plain_reload_refreshes_external_state_once_at_next_prompt() {
    let sb = Sandbox::new();
    sb.write(".cade", "call /bin/sh counter.sh\n");
    sb.write(
        "counter.sh",
        "n=0; if [ -f count ]; then read -r n < count; fi; n=$((n+1)); printf '%s\\n' \"$n\" > \
         count; printf 'VALUE=%s\\n' \"$n\"\n",
    );
    sb.allow(&sb.root);
    let out = shell(
        &sb,
        r#"
eval "$("$CADE_BIN" hook bash)"
_cade_hook
printf 'initial=%s\n' "$VALUE"
_cade_hook
printf 'unchanged=%s\n' "$VALUE"
"$CADE_BIN" reload
_cade_hook
printf 'refreshed=%s\n' "$VALUE"
_cade_hook
printf 'settled=%s\n' "$VALUE"
"#,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "initial=1\nunchanged=1\nrefreshed=2\nsettled=2\n"
    );
    let fresh_shell = sb.run(&sb.root, &["reload", "--shell", "bash"], &[]);
    assert!(fresh_shell.status.success(), "{}", stderr(&fresh_shell));
    assert!(
        stdout(&fresh_shell).contains("export VALUE='2';"),
        "{}",
        stdout(&fresh_shell)
    );
}

#[test]
fn plain_reload_retries_an_unchanged_failed_loader() {
    let sb = Sandbox::new();
    sb.write(".cade", "call /bin/sh loader.sh\n");
    sb.write(
        "loader.sh",
        "test -f ready || exit 1; printf 'VALUE=recovered\\n'\n",
    );
    sb.allow(&sb.root);
    let out = shell(
        &sb,
        r#"
eval "$("$CADE_BIN" hook bash)"
_cade_hook
: > ready
_cade_hook
printf 'before=%s\n' "${VALUE-unset}"
"$CADE_BIN" reload
_cade_hook
printf 'after=%s\n' "${VALUE-unset}"
"#,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "before=unset\nafter=recovered\n");
}

#[test]
fn plain_reload_refreshes_every_approved_layer() {
    let sb = Sandbox::new();
    sb.write(".cade", "call /bin/sh counter.sh\n");
    sb.write(
        "counter.sh",
        "n=0; if [ -f count ]; then read -r n < count; fi; n=$((n+1)); printf '%s\\n' \"$n\" > \
         count; printf 'PARENT_COUNT=%s\\n' \"$n\"\n",
    );
    let child = sb.dir("child");
    sb.write("child/.cade", "call /bin/sh counter.sh\n");
    sb.write(
        "child/counter.sh",
        "n=0; if [ -f count ]; then read -r n < count; fi; n=$((n+1)); printf '%s\\n' \"$n\" > \
         count; printf 'CHILD_COUNT=%s\\n' \"$n\"\n",
    );
    sb.allow(&sb.root);
    sb.allow(&child);
    let out = shell(
        &sb,
        r#"
cd child
eval "$("$CADE_BIN" hook bash)"
_cade_hook
printf 'initial=%s:%s\n' "$PARENT_COUNT" "$CHILD_COUNT"
"$CADE_BIN" reload
_cade_hook
printf 'refreshed=%s:%s\n' "$PARENT_COUNT" "$CHILD_COUNT"
"#,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "initial=1:1\nrefreshed=2:2\n");
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
