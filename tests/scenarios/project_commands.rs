use std::{
    env::var,
    fs::{
        create_dir_all,
        read_to_string,
        remove_file,
        write,
    },
    path::Path,
    process::Output,
};
#[cfg(unix)]
use std::{
    fs::set_permissions,
    os::unix::fs::symlink,
};

use common::{
    Sandbox,
    stderr,
    stdout,
};

use super::common;

fn enter(sb: &Sandbox, cwd: &Path) -> Output {
    sb.run(cwd, &["enter", "--shell", "bash"], &[])
}

fn editor(sb: &Sandbox, body: &str) -> String {
    let script = sb.state.join("editor script.sh");
    write(&script, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    format!("/bin/sh '{}' 'editor argument'", script.display())
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
fn allow_accepts_relative_directory_and_explicit_effective_file_paths() {
    let sb = Sandbox::new();
    sb.write("project/.cade", "VALUE=target\n");
    sb.dir("project/deep");
    let file_allow = sb.run(&sb.root, &["allow", "project/deep/../.cade"], &[]);
    assert!(file_allow.status.success(), "{file_allow:?}");
    assert!(stdout(&enter(&sb, &sb.root.join("project/deep"))).contains("export VALUE='target';"));

    let disallow = sb.run(&sb.root, &["disallow", "project/deep"], &[]);
    assert!(disallow.status.success(), "{disallow:?}");
    assert_disallowed(&enter(&sb, &sb.root.join("project")));
    let directory_allow = sb.run(&sb.root, &["allow", "project/deep"], &[]);
    assert!(directory_allow.status.success(), "{directory_allow:?}");
    assert!(stdout(&enter(&sb, &sb.root.join("project"))).contains("export VALUE='target';"));
}

#[test]
fn allow_rejects_shadowed_explicit_envrc_without_approving_cade() {
    let sb = Sandbox::new();
    sb.write(".cade", "VALUE=cade\n");
    sb.write(".envrc", "export VALUE=envrc\n");
    let out = sb.run(&sb.root, &["allow", ".envrc"], &[]);
    assert!(!out.status.success(), "{out:?}");
    assert!(stderr(&out).contains("not the effective"), "{out:?}");
    assert_disallowed(&enter(&sb, &sb.root));
}

#[cfg(unix)]
#[test]
fn allow_canonicalizes_directory_symlinks() {
    let sb = Sandbox::new();
    sb.write(".cade", "VALUE=canonical\n");
    let alias = sb.state.join("alias");
    symlink(&sb.root, &alias).unwrap();
    let out = sb.run(&sb.root, &["allow", alias.to_str().unwrap()], &[]);
    assert!(out.status.success(), "{out:?}");
    assert!(stdout(&enter(&sb, &sb.root)).contains("export VALUE='canonical';"));
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

#[test]
fn edit_from_descendant_targets_root_and_prefers_visual_with_split_arguments() {
    let sb = Sandbox::new();
    sb.write(".cade", "VALUE=old\n");
    let deep = sb.dir("a/b");
    let command = editor(
        &sb,
        "test \"$1\" = 'editor argument'\ntest \"$PWD\" = \"${2%/*}\"\nprintf 'VALUE=edited\\n' > \
         \"$2\"",
    );
    let out = sb.run(&deep, &["edit"], &[
        ("VISUAL", &command),
        ("EDITOR", "/bin/false"),
    ]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        read_to_string(sb.root.join(".cade")).unwrap(),
        "VALUE=edited\n"
    );
    assert!(!deep.join(".cade").exists());
    assert!(stdout(&enter(&sb, &deep)).contains("export VALUE='edited';"));
}

#[test]
fn edit_envrc_only_project_edits_envrc_and_falls_back_to_editor() {
    let sb = Sandbox::new();
    sb.write(".envrc", "export VALUE=old\n");
    let deep = sb.dir("deep");
    let command = editor(
        &sb,
        "test \"${2##*/}\" = .envrc\nprintf 'export VALUE=edited\\n' > \"$2\"",
    );
    let out = sb.run(&deep, &["edit"], &[("VISUAL", " "), ("EDITOR", &command)]);
    assert!(out.status.success(), "{out:?}");
    assert!(!sb.root.join(".cade").exists());
    assert_eq!(
        read_to_string(sb.root.join(".envrc")).unwrap(),
        "export VALUE=edited\n"
    );
    assert!(stdout(&enter(&sb, &deep)).contains("export VALUE='edited';"));
}

#[test]
fn edit_creates_and_approves_cade_in_requested_directory() {
    let sb = Sandbox::new();
    let target = sb.dir("new project");
    let command = editor(&sb, "test -f \"$2\"\nprintf 'VALUE=created\\n' > \"$2\"");
    let out = sb.run(&sb.root, &["edit", "new project"], &[("EDITOR", &command)]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        read_to_string(target.join(".cade")).unwrap(),
        "VALUE=created\n"
    );
    assert!(!sb.root.join(".cade").exists());
    assert!(stdout(&enter(&sb, &target)).contains("export VALUE='created';"));
}

#[test]
fn edit_nonzero_exit_does_not_approve_written_content() {
    let sb = Sandbox::new();
    sb.write(".cade", "VALUE=old\n");
    sb.allow(&sb.root);
    let command = editor(&sb, "printf 'VALUE=unapproved\\n' > \"$2\"\nexit 7");
    let out = sb.run(&sb.root, &["edit"], &[("EDITOR", &command)]);
    assert!(!out.status.success(), "{out:?}");
    assert!(
        stderr(&out).contains("editor exited unsuccessfully"),
        "{out:?}"
    );
    assert_disallowed(&enter(&sb, &sb.root));
}

#[test]
fn edit_success_without_a_remaining_config_does_not_approve() {
    let sb = Sandbox::new();
    sb.write(".cade", "VALUE=old\n");
    let command = editor(&sb, "rm -- \"$2\"");
    let path = var("PATH").unwrap();
    let out = sb.run(&sb.root, &["edit"], &[
        ("EDITOR", &command),
        ("PATH", &path),
    ]);
    assert!(!out.status.success(), "{out:?}");
    sb.write(".cade", "VALUE=unapproved\n");
    assert_disallowed(&enter(&sb, &sb.root));
}

#[test]
fn edit_does_not_approve_a_different_config_created_by_editor() {
    let sb = Sandbox::new();
    sb.write(".envrc", "export VALUE=old\n");
    let command = editor(&sb, "printf 'VALUE=unapproved\\n' > \"${2%/*}/.cade\"");
    let out = sb.run(&sb.root, &["edit"], &[("EDITOR", &command)]);
    assert!(!out.status.success(), "{out:?}");
    assert_disallowed(&enter(&sb, &sb.root));
}

#[test]
fn edit_without_an_existing_project_creates_cade_in_cwd() {
    let sb = Sandbox::new();
    let command = editor(&sb, "printf 'VALUE=created\\n' > \"$2\"");
    let out = sb.run(&sb.root, &["edit"], &[("EDITOR", &command)]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        read_to_string(sb.root.join(".cade")).unwrap(),
        "VALUE=created\n"
    );
    assert!(stdout(&enter(&sb, &sb.root)).contains("export VALUE='created';"));
}

#[test]
fn edit_accepts_an_explicit_effective_config_file() {
    let sb = Sandbox::new();
    sb.write("project/.envrc", "export VALUE=old\n");
    let command = editor(
        &sb,
        "test \"${2##*/}\" = .envrc\nprintf 'export VALUE=explicit\\n' > \"$2\"",
    );
    let out = sb.run(&sb.root, &["edit", "project/.envrc"], &[(
        "EDITOR", &command,
    )]);
    assert!(out.status.success(), "{out:?}");
    assert!(stdout(&enter(&sb, &sb.root.join("project"))).contains("export VALUE='explicit';"));
}

#[cfg(unix)]
#[test]
fn edit_uses_vi_when_editor_variables_are_absent() {
    use std::os::unix::fs::PermissionsExt as _;

    let sb = Sandbox::new();
    let bin = sb.state.join("bin");
    create_dir_all(&bin).unwrap();
    let vi = bin.join("vi");
    write(&vi, "#!/bin/sh\nprintf 'VALUE=vi\\n' > \"$1\"\n").unwrap();
    let mut permissions = vi.metadata().unwrap().permissions();
    permissions.set_mode(0o755);
    set_permissions(&vi, permissions).unwrap();
    let out = sb.run(&sb.root, &["edit"], &[("PATH", bin.to_str().unwrap())]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(read_to_string(sb.root.join(".cade")).unwrap(), "VALUE=vi\n");
    assert!(stdout(&enter(&sb, &sb.root)).contains("export VALUE='vi';"));
}
