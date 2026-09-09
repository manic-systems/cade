use std::{
    path::Path,
    process::{
        Command,
        Stdio,
    },
};

use crate::verbosity::{
    Verbosity,
    log,
};

pub(super) fn wipe_history(profile: &Path) {
    let spawn_result = Command::new("nix")
        .args(["profile", "wipe-history", "--profile"])
        .arg(profile)
        .stdout(Stdio::null())
        .status();
    match spawn_result {
        Ok(exit_status) if exit_status.success() => {},
        Ok(exit_status) => {
            log(
                Verbosity::Trace,
                format_args!(
                    "cade: failed to wipe nix profile history for {} ({exit_status}).",
                    profile.display()
                ),
            );
        },
        Err(error) => {
            log(
                Verbosity::Trace,
                format_args!(
                    "cade: failed to run nix profile wipe-history for {}: {error}.",
                    profile.display()
                ),
            );
        },
    }
}
