use crate::types::LoadSpec;
use std::path::{Path, PathBuf};

const FLAKE_WATCH_EXCLUDED_DIRS: &[&str] = &[
    ".git",
    ".jj",
    ".hg",
    ".svn",
    ".direnv",
    "node_modules",
    "target",
    "outputs",
];

pub struct FlakeTarget {
    pub cwd: PathBuf,
    pub installable: String,
    pub spec: LoadSpec,
}

impl FlakeTarget {
    pub fn bare_output(dir: &Path, output: Option<&str>) -> Self {
        match output.filter(|o| !o.is_empty()) {
            Some(o) => FlakeTarget {
                cwd: dir.to_path_buf(),
                installable: format!(".#{o}"),
                spec: LoadSpec::FlakeOutput(o.to_string()),
            },
            None => FlakeTarget {
                cwd: dir.to_path_buf(),
                installable: String::new(),
                spec: LoadSpec::FlakeDefault,
            },
        }
    }
}

fn looks_like_path(arg: &str) -> bool {
    arg.contains('#')
        || arg.contains('/')
        || arg.starts_with('.')
        || arg.starts_with('~')
        || arg.starts_with('/')
}

pub fn resolve_flake_target(layer_dir: &Path, arg: Option<&str>) -> FlakeTarget {
    let Some(arg) = arg.filter(|a| !a.is_empty()) else {
        return FlakeTarget::bare_output(layer_dir, None);
    };

    if !looks_like_path(arg) {
        return FlakeTarget::bare_output(layer_dir, Some(arg));
    }

    let (path_part, output) = match arg.split_once('#') {
        Some((p, o)) => (p, Some(o)),
        None => (arg, None),
    };
    let path_part = if path_part.is_empty() { "." } else { path_part };
    let dir = crate::path_resolve::resolve_for_watch(layer_dir, path_part);
    let installable = match output {
        Some(o) if !o.is_empty() => format!("{}#{o}", dir.display()),
        _ => dir.display().to_string(),
    };
    FlakeTarget {
        cwd: dir,
        spec: LoadSpec::FlakeInstallable(installable.clone()),
        installable,
    }
}

pub fn flake_watch_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_flake_watch_files(root, &mut files);
    files.push(root.join("flake.nix"));
    files.push(root.join("flake.lock"));
    files.sort_unstable();
    files.dedup();
    files
}

fn collect_flake_watch_files(path: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let child = entry.path();
        let file_name = child.file_name().and_then(|name| name.to_str());
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            let excluded = file_name.is_some_and(|name| FLAKE_WATCH_EXCLUDED_DIRS.contains(&name));
            if !excluded {
                collect_flake_watch_files(&child, out);
            }
        } else if (file_type.is_file() || file_type.is_symlink())
            && !file_name.is_some_and(is_nix_result_link)
        {
            out.push(child);
        }
    }
}

fn is_nix_result_link(name: &str) -> bool {
    name == "result" || name.starts_with("result-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_output_stays_current_dir_installable() {
        let layer = Path::new("/layer");
        let target = resolve_flake_target(layer, Some("dev"));
        assert_eq!(target.installable, ".#dev");
        assert_eq!(target.cwd, layer);
        assert_eq!(target.spec.cache_key(), "flake:dev");
    }

    #[test]
    fn no_arg_is_current_dir_default() {
        let layer = Path::new("/layer");
        let target = resolve_flake_target(layer, None);
        assert!(target.installable.is_empty());
        assert_eq!(target.cwd, layer);
        assert_eq!(target.spec.cache_key(), "flake");
    }

    #[test]
    fn directed_flake_path_resolves_and_runs_in_target_dir() {
        let base = std::env::temp_dir().join(format!(
            "cade-flake-target-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let sub = base.join("svc");
        std::fs::create_dir_all(&sub).unwrap();
        let canon_sub = std::fs::canonicalize(&sub).unwrap();

        let target = resolve_flake_target(&base, Some("./svc#dev"));
        assert_eq!(target.cwd, canon_sub);
        assert_eq!(target.installable, format!("{}#dev", canon_sub.display()));

        let target = resolve_flake_target(&base, Some("./svc"));
        assert_eq!(target.cwd, canon_sub);
        assert_eq!(target.installable, canon_sub.display().to_string());

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn directed_flake_missing_path_watches_real_target() {
        let layer = Path::new("/no/such/layer");
        let target = resolve_flake_target(layer, Some("./nope"));
        assert_eq!(target.cwd, Path::new("/no/such/layer/nope"));
        assert_eq!(target.installable, "/no/such/layer/nope");
        assert_eq!(target.spec.cache_key(), "flake:/no/such/layer/nope");
    }

    #[test]
    fn flake_watch_includes_local_imports_and_excludes_build_outputs() {
        let root = std::env::temp_dir().join(format!(
            "cade-flake-watch-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(root.join(".tack")).unwrap();
        std::fs::create_dir_all(root.join(".jj")).unwrap();
        std::fs::create_dir_all(root.join("nix")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("flake.nix"), "").unwrap();
        std::fs::write(root.join(".jj").join("repo"), "").unwrap();
        std::fs::write(root.join(".tack").join("default.nix"), "").unwrap();
        std::fs::write(root.join("nix").join("package.nix"), "").unwrap();
        std::fs::write(root.join("result"), "").unwrap();
        std::fs::write(root.join("result-dev"), "").unwrap();
        std::fs::write(root.join("target").join("generated.nix"), "").unwrap();

        let watch = flake_watch_files(&root);

        assert!(watch.contains(&root.join("flake.nix")));
        assert!(watch.contains(&root.join(".tack").join("default.nix")));
        assert!(watch.contains(&root.join("nix").join("package.nix")));
        assert!(!watch.contains(&root));
        assert!(!watch.contains(&root.join(".jj").join("repo")));
        assert!(!watch.contains(&root.join("result")));
        assert!(!watch.contains(&root.join("result-dev")));
        assert!(!watch.contains(&root.join("target").join("generated.nix")));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn flake_watch_tracks_missing_flake_files_without_watching_root_dir() {
        let root = std::env::temp_dir().join(format!(
            "cade-flake-watch-missing-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".envrc"), "use flake\n").unwrap();

        let watch = flake_watch_files(&root);

        assert!(watch.contains(&root.join("flake.nix")));
        assert!(watch.contains(&root.join("flake.lock")));
        assert!(!watch.contains(&root));

        std::fs::remove_dir_all(&root).ok();
    }
}
