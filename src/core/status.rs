use crate::core::Cade;
use crate::core::participants::find_cade_root;
use crate::core::participants::participant_dirs;
use crate::core::permissions::get_permission;
use crate::core::shell_state::ShellState;
use anyhow::Result;

pub fn do_status(cade: &Cade) -> Result<()> {
    let root = find_cade_root(&cade.cwd);
    let shell_state = ShellState::from_env();

    println!("cwd:     {}", cade.cwd.display());
    match root.as_ref() {
        Some(project_root) => {
            println!("root:    {}", project_root.display());
            println!("layers (inner \u{2192} outer):");
            let mut capped = false;
            for dir in participant_dirs(project_root) {
                let allowed = get_permission(cade, &dir)?;
                if !allowed {
                    capped = true;
                }
                let mark = if !allowed {
                    "not allowed  (run 'cade allow' here)"
                } else if capped {
                    "allowed, but excluded (a lower layer is not allowed)"
                } else {
                    "allowed, composed"
                };
                println!("  {}  [{mark}]", dir.display());
            }
        }
        None => println!("root:    none (not in a cade project)"),
    }

    println!(
        "active:  {}",
        if shell_state.is_active() { "yes" } else { "no" }
    );
    if shell_state.is_active() {
        if !shell_state.set_keys().is_empty() {
            println!("set:     {}", shell_state.set_keys().join(", "));
        }
        if !shell_state.unset_keys().is_empty() {
            println!("cleared: {}", shell_state.unset_keys().join(", "));
        }
    }
    Ok(())
}
