use super::{Cade, find_cade_root, participant_dirs, shell_state::ShellState};
use anyhow::Result;

impl Cade {
    pub fn do_status(&mut self) -> Result<()> {
        let root = find_cade_root(&self.cwd);
        let shell_state = ShellState::from_env();

        println!("cwd:     {}", self.cwd.display());
        match &root {
            Some(r) => {
                println!("root:    {}", r.display());
                println!("layers (inner \u{2192} outer):");
                let mut capped = false;
                for dir in participant_dirs(r) {
                    let allowed = self.get_permission(&dir)?;
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
}
