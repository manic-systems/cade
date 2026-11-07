use std::{
    fs::{
        create_dir_all,
        remove_file,
    },
    path::Path,
    process::Output,
};

use super::common::{
    Sandbox,
    stderr,
    stdout,
};

fn enter(sb: &Sandbox, cwd: &Path) -> Output {
    sb.run(cwd, &["enter", "--shell", "bash"], &[])
}

fn assert_disallowed(out: &Output) {
    assert!(!out.status.success(), "unexpected activation: {out:?}");
    assert!(stderr(out).contains("disallowed"), "{out:?}");
}

#[test]
fn changed_cade_requires_reapproval_even_when_length_is_unchanged() {
    let sb = Sandbox::new();
    sb.write(".cade", "VALUE=old\n");
    sb.allow(&sb.root);
    assert!(stdout(&enter(&sb, &sb.root)).contains("export VALUE='old';"));

    sb.write(".cade", "VALUE=new\n");
    assert_disallowed(&enter(&sb, &sb.root));
    sb.allow(&sb.root);
    assert!(stdout(&enter(&sb, &sb.root)).contains("export VALUE='new';"));
}

#[test]
fn changed_envrc_requires_reapproval() {
    let sb = Sandbox::new();
    sb.write(".envrc", "export VALUE=old\n");
    sb.allow(&sb.root);
    assert!(stdout(&enter(&sb, &sb.root)).contains("export VALUE='old';"));

    sb.write(".envrc", "export VALUE=new\n");
    assert_disallowed(&enter(&sb, &sb.root));
    sb.allow(&sb.root);
    assert!(stdout(&enter(&sb, &sb.root)).contains("export VALUE='new';"));
}

#[test]
fn switching_effective_config_requires_reapproval_even_with_identical_bytes() {
    let sb = Sandbox::new();
    sb.write(".envrc", "VALUE=same\n");
    sb.allow(&sb.root);
    sb.write(".cade", "VALUE=same\n");
    assert_disallowed(&enter(&sb, &sb.root));

    sb.allow(&sb.root);
    assert!(stdout(&enter(&sb, &sb.root)).contains("export VALUE='same';"));
    remove_file(sb.root.join(".cade")).unwrap();
    assert_disallowed(&enter(&sb, &sb.root));
}

#[test]
fn legacy_path_only_approval_fails_closed_until_reapproved() {
    let sb = Sandbox::new();
    sb.write(".cade", "VALUE=trusted\n");
    let state = sb.state.join("cade");
    create_dir_all(&state).unwrap();
    let db = rusqlite::Connection::open(state.join("cade.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE WorkingPaths (Path TEXT PRIMARY KEY, Permission INTEGER NOT NULL DEFAULT 0);",
    )
    .unwrap();
    db.execute(
        "INSERT INTO WorkingPaths (Path, Permission) VALUES (?1, 1)",
        [sb.root.canonicalize().unwrap().to_str().unwrap()],
    )
    .unwrap();
    drop(db);

    assert_disallowed(&enter(&sb, &sb.root));
    sb.allow(&sb.root);
    assert!(stdout(&enter(&sb, &sb.root)).contains("export VALUE='trusted';"));
}

#[test]
fn allow_fills_new_ancestor_gaps_to_an_unchanged_approved_base() {
    let sb = Sandbox::new();
    sb.write(".cade", "BASE=base\n");
    sb.write("middle/.cade", "MIDDLE=middle\n");
    sb.write("middle/deep/.cade", "TIP=tip\n");
    sb.allow(&sb.root);
    let deep = sb.root.join("middle/deep");
    sb.allow(&deep);
    let out = enter(&sb, &deep);
    assert!(out.status.success(), "{out:?}");
    let script = stdout(&out);
    assert!(script.contains("export BASE='base';"), "{script}");
    assert!(script.contains("export MIDDLE='middle';"), "{script}");
    assert!(script.contains("export TIP='tip';"), "{script}");
}

#[test]
fn allowing_a_child_does_not_reapprove_a_changed_parent() {
    let sb = Sandbox::new();
    sb.write(".cade", "BASE=base\n");
    sb.write("middle/.cade", "MIDDLE=old\n");
    sb.write("middle/deep/.cade", "TIP=tip\n");
    sb.allow(&sb.root);
    let middle = sb.root.join("middle");
    sb.allow(&middle);
    sb.write("middle/.cade", "MIDDLE=new\n");
    let deep = middle.join("deep");
    sb.allow(&deep);
    let out = enter(&sb, &deep);
    assert!(out.status.success(), "{out:?}");
    let script = stdout(&out);
    assert!(script.contains("export TIP='tip';"), "{script}");
    assert!(!script.contains("export MIDDLE="), "{script}");
    assert!(!script.contains("export BASE="), "{script}");
    let parent = enter(&sb, &middle);
    assert!(parent.status.success(), "{parent:?}");
    assert!(!stdout(&parent).contains("export MIDDLE="), "{parent:?}");
    assert!(
        stdout(&parent).contains("export BASE='base';"),
        "{parent:?}"
    );
}
