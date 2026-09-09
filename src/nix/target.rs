use std::path::{
    Path,
    PathBuf,
};

use crate::{
    path_resolve::resolve_for_watch,
    types::load_spec::LoadSpec,
};

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

// Dependency manifests nix may read to build a dev shell
const FLAKE_WATCH_MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "rust-toolchain",
    "rust-toolchain.toml",
    "go.mod",
    "go.sum",
    "gomod2nix.toml",
    "package.json",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "bun.lock",
    "bun.lockb",
    "pyproject.toml",
    "requirements.txt",
    "Pipfile",
    "setup.py",
    "setup.cfg",
    "composer.json",
    "Gemfile",
    "mix.exs",
    "pubspec.yaml",
    "stack.yaml",
    "package.yaml",
    "cabal.project",
    "cabal.project.freeze",
    "shard.yml",
    "build.zig.zon",
    "packages.lock.json",
    "pom.xml",
    "dune-project",
    "Package.swift",
    "Package.resolved",
    "cpanfile",
    "rebar.config",
];

fn is_flake_input_file(name: &str) -> bool {
    matches!(
        Path::new(name).extension().and_then(|ext| ext.to_str()),
        Some("nix" | "lock" | "cabal" | "opam")
    ) || FLAKE_WATCH_MANIFESTS.contains(&name)
}

pub struct FlakeTarget {
    pub cwd:         PathBuf,
    pub installable: String,
    pub spec:        LoadSpec,
}

impl FlakeTarget {
    pub fn bare_output(dir: &Path, output: Option<&str>) -> Self {
        output.filter(|text| !text.is_empty()).map_or_else(
            || {
                Self {
                    cwd:         dir.to_path_buf(),
                    installable: String::new(),
                    spec:        LoadSpec::FlakeDefault,
                }
            },
            |output_name| {
                Self {
                    cwd:         dir.to_path_buf(),
                    installable: format!(".#{output_name}"),
                    spec:        LoadSpec::FlakeOutput(output_name.to_owned()),
                }
            },
        )
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
    let Some(target_arg) = arg.filter(|text| !text.is_empty()) else {
        return FlakeTarget::bare_output(layer_dir, None);
    };

    if !looks_like_path(target_arg) {
        return FlakeTarget::bare_output(layer_dir, Some(target_arg));
    }

    let (path_part, output) = match target_arg.split_once('#') {
        Some((path_text, output_text)) => (path_text, Some(output_text)),
        None => (target_arg, None),
    };
    let resolved_part = if path_part.is_empty() { "." } else { path_part };
    let dir = resolve_for_watch(layer_dir, resolved_part);
    let installable = match output {
        Some(fragment) if !fragment.is_empty() => format!("{}#{fragment}", dir.display()),
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

// Nix reads only VCS-tracked files when evaluating a local flake, so an ignored
// subtree cannot affect the dev shell.
fn collect_flake_watch_files(root: &Path, out: &mut Vec<PathBuf>) {
    let walk = ignore::WalkBuilder::new(root)
        .hidden(false)
        .ignore(false)
        .require_git(false)
        .filter_entry(|entry| entry.depth() == 0 || !is_excluded_dir(entry))
        .build();

    for entry in walk.flatten() {
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() && entry.file_name().to_str().is_some_and(is_flake_input_file) {
            out.push(entry.into_path());
        }
    }
}

fn is_excluded_dir(entry: &ignore::DirEntry) -> bool {
    entry.file_type().is_some_and(|ty| ty.is_dir())
        && entry
            .file_name()
            .to_str()
            .is_some_and(|name| FLAKE_WATCH_EXCLUDED_DIRS.contains(&name))
}

#[cfg(test)]
mod tests {
    use std::{
        env::temp_dir,
        fs::{
            canonicalize,
            create_dir_all,
            remove_dir_all,
            write,
        },
        process::id,
        thread::current,
    };

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
        let base = temp_dir().join(format!(
            "cade-flake-target-{}-{}",
            id(),
            current().name().unwrap_or("test")
        ));
        let sub = base.join("svc");
        create_dir_all(&sub).unwrap();
        let canon_sub = canonicalize(&sub).unwrap();

        let dev_target = resolve_flake_target(&base, Some("./svc#dev"));
        assert_eq!(dev_target.cwd, canon_sub);
        assert_eq!(
            dev_target.installable,
            format!("{}#dev", canon_sub.display())
        );

        let path_target = resolve_flake_target(&base, Some("./svc"));
        assert_eq!(path_target.cwd, canon_sub);
        assert_eq!(path_target.installable, canon_sub.display().to_string());

        let _ = remove_dir_all(&base);
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
        let root = temp_dir().join(format!(
            "cade-flake-watch-{}-{}",
            id(),
            current().name().unwrap_or("test")
        ));
        create_dir_all(root.join(".tack")).unwrap();
        create_dir_all(root.join(".jj")).unwrap();
        create_dir_all(root.join("nix")).unwrap();
        create_dir_all(root.join("target")).unwrap();
        write(root.join("flake.nix"), "").unwrap();
        write(root.join(".jj").join("repo"), "").unwrap();
        write(root.join(".tack").join("default.nix"), "").unwrap();
        write(root.join("nix").join("package.nix"), "").unwrap();
        write(root.join("result"), "").unwrap();
        write(root.join("result-dev"), "").unwrap();
        write(root.join("target").join("generated.nix"), "").unwrap();
        // dependency manifests nix may read still need watching
        write(root.join("Cargo.lock"), "").unwrap();
        write(root.join("gomod2nix.toml"), "").unwrap();
        write(root.join("nix").join("requirements.txt"), "").unwrap();
        // plain source is irrelevant to the dev env and must not be watched
        write(root.join("main.cpp"), "").unwrap();
        write(root.join("nix").join("notes.md"), "").unwrap();

        let watch = flake_watch_files(&root);

        assert!(watch.contains(&root.join("flake.nix")));
        assert!(watch.contains(&root.join(".tack").join("default.nix")));
        assert!(watch.contains(&root.join("nix").join("package.nix")));
        assert!(watch.contains(&root.join("Cargo.lock")));
        assert!(watch.contains(&root.join("gomod2nix.toml")));
        assert!(watch.contains(&root.join("nix").join("requirements.txt")));
        assert!(!watch.contains(&root));
        assert!(!watch.contains(&root.join(".jj").join("repo")));
        assert!(!watch.contains(&root.join("result")));
        assert!(!watch.contains(&root.join("result-dev")));
        assert!(!watch.contains(&root.join("target").join("generated.nix")));
        assert!(!watch.contains(&root.join("main.cpp")));
        assert!(!watch.contains(&root.join("nix").join("notes.md")));

        let _ = remove_dir_all(&root);
    }

    #[test]
    fn flake_watch_skips_vcs_ignored_trees() {
        let root = temp_dir().join(format!(
            "cade-flake-watch-ignored-{}-{}",
            id(),
            current().name().unwrap_or("test")
        ));
        create_dir_all(root.join("out").join("deep")).unwrap();
        create_dir_all(root.join("nix")).unwrap();
        write(root.join(".gitignore"), "out/\n").unwrap();
        write(root.join("flake.nix"), "").unwrap();
        write(root.join("nix").join("package.nix"), "").unwrap();
        write(root.join("out").join("deep").join("generated.nix"), "").unwrap();

        let watch = flake_watch_files(&root);

        assert!(watch.contains(&root.join("nix").join("package.nix")));
        assert!(!watch.contains(&root.join("out").join("deep").join("generated.nix")));

        let _ = remove_dir_all(&root);
    }

    #[test]
    fn flake_watch_tracks_missing_flake_files_without_watching_root_dir() {
        let root = temp_dir().join(format!(
            "cade-flake-watch-missing-{}-{}",
            id(),
            current().name().unwrap_or("test")
        ));
        create_dir_all(&root).unwrap();
        write(root.join(".envrc"), "use flake\n").unwrap();

        let watch = flake_watch_files(&root);

        assert!(watch.contains(&root.join("flake.nix")));
        assert!(watch.contains(&root.join("flake.lock")));
        assert!(!watch.contains(&root));

        let _ = remove_dir_all(&root);
    }
}
