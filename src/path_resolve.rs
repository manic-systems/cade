//! resolve loader target paths against a layer's own directory
//!
//! relative paths resolve against the layer dir; `.`, `..`, absolute paths, and
//! a leading `~/` (expanded to $HOME) are all accepted. trust is the allowed
//! layer dir: a directed target is still just a nix/dotenv source inside an
//! already-approved layer, never a cade layer of its own

use std::path::{Path, PathBuf};

/// expand a leading `~`/`~/...` to $HOME; other uses of `~` are left untouched,
/// matching the conservative expansion shells apply
fn expand_tilde(arg: &str) -> PathBuf {
    expand_tilde_with(arg, home_dir())
}

fn expand_tilde_with(arg: &str, home: Option<PathBuf>) -> PathBuf {
    if arg == "~" {
        if let Some(home) = home {
            return home;
        }
    } else if let Some(rest) = arg.strip_prefix("~/")
        && let Some(home) = home
    {
        return home.join(rest);
    }
    PathBuf::from(arg)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// resolve `arg` against the layer dir without requiring the result to exist.
/// absolute and `~`-prefixed args are taken as is, everything else joins onto
/// `layer_dir`. normalised lexically, not canonicalised, so it works for
/// not-yet-created paths
pub fn resolve_against(layer_dir: &Path, arg: &str) -> PathBuf {
    let expanded = expand_tilde(arg);
    let joined = if expanded.is_absolute() {
        expanded
    } else {
        layer_dir.join(expanded)
    };
    normalize_lexical(&joined)
}

/// watch-set identity: the canonical path when the target exists (matching the
/// gc-root/spec key the load path derives), else the lexical path so creating
/// the file still trips the watcher
pub fn resolve_for_watch(layer_dir: &Path, arg: &str) -> PathBuf {
    std::fs::canonicalize(resolve_against(layer_dir, arg))
        .unwrap_or_else(|_| resolve_against(layer_dir, arg))
}

/// collapse `.` and `..` lexically, leaving symlinks alone, so a target's path
/// is stable for hashing/watching even before it exists
fn normalize_lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                // keep `..` that can't be collapsed
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) => {}
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_joins_layer_dir() {
        assert_eq!(
            resolve_against(Path::new("/layer"), "sub"),
            PathBuf::from("/layer/sub")
        );
        assert_eq!(
            resolve_against(Path::new("/layer"), "./sub"),
            PathBuf::from("/layer/sub")
        );
    }

    #[test]
    fn parent_and_absolute() {
        assert_eq!(
            resolve_against(Path::new("/layer/inner"), "../svc"),
            PathBuf::from("/layer/svc")
        );
        assert_eq!(
            resolve_against(Path::new("/layer"), "/abs/path"),
            PathBuf::from("/abs/path")
        );
    }

    #[test]
    fn watch_canonicalises_through_symlink() {
        // the watch set and the gc-root/spec key both derive from
        // resolve_for_watch, so the same physical file reached via a symlink
        // shares one identity and an edit through it trips the watcher
        let base = std::env::temp_dir().join(format!(
            "cade-symlink-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let real = base.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join(".env"), "X=1\n").unwrap();
        let link = base.join("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&real, &link).unwrap();

        // both the symlinked dir and the real dir resolve to one canonical file
        let via_link = resolve_for_watch(&link, ".env");
        let via_real = resolve_for_watch(&real, ".env");
        assert_eq!(via_link, via_real);
        assert_eq!(via_link, std::fs::canonicalize(real.join(".env")).unwrap());

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn watch_falls_back_to_lexical_when_missing() {
        // a not-yet-created file still gets a watch entry so creating it reloads
        assert_eq!(
            resolve_for_watch(Path::new("/no/such/layer"), "shell.nix"),
            PathBuf::from("/no/such/layer/shell.nix")
        );
    }

    #[test]
    fn tilde_expands_to_home() {
        let home = Some(PathBuf::from("/home/tester"));
        assert_eq!(
            expand_tilde_with("~/proj", home.clone()),
            PathBuf::from("/home/tester/proj")
        );
        assert_eq!(expand_tilde_with("~", home), PathBuf::from("/home/tester"));
        // bare `~user` (no slash) left untouched, like shells
        assert_eq!(
            expand_tilde_with("~other/x", Some(PathBuf::from("/home/tester"))),
            PathBuf::from("~other/x")
        );
    }
}
