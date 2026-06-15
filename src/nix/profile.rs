use std::{
    path::Path,
    process::{Command, Stdio},
};

pub(super) fn wipe_history(profile: &Path) {
    let status = Command::new("nix")
        .args(["profile", "wipe-history", "--profile"])
        .arg(profile)
        .stdout(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => crate::verbosity::log(
            crate::verbosity::Verbosity::Trace,
            format_args!(
                "cade: failed to wipe nix profile history for {} ({status}).",
                profile.display()
            ),
        ),
        Err(e) => crate::verbosity::log(
            crate::verbosity::Verbosity::Trace,
            format_args!(
                "cade: failed to run nix profile wipe-history for {}: {e}.",
                profile.display()
            ),
        ),
    }
}
