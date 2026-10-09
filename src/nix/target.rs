use std::{
    ffi::OsStr,
    fs::{
        read,
        read_dir,
        read_to_string,
    },
    os::unix::ffi::OsStrExt as _,
    path::{
        Path,
        PathBuf,
    },
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
    let Some(tracked) = tracked_files(root) else {
        walk_flake_dir(root, out);
        return;
    };

    out.extend(tracked.into_iter().filter(|path| {
        path.file_name()
            .and_then(OsStr::to_str)
            .is_some_and(is_flake_input_file)
    }));
}

fn tracked_files(root: &Path) -> Option<Vec<PathBuf>> {
    let (top, dot_git) = root.ancestors().find_map(|dir| {
        let candidate = dir.join(".git");
        candidate.exists().then_some((dir, candidate))
    })?;
    let git_dir = if dot_git.is_file() {
        let pointer = read_to_string(&dot_git).ok()?;
        top.join(pointer.strip_prefix("gitdir:")?.trim())
    } else {
        dot_git
    };
    let prefix = root.strip_prefix(top).ok()?;
    let index = read(git_dir.join("index")).ok()?;

    Some(
        parse_index(&index)?
            .iter()
            .filter_map(|path| path.strip_prefix(prefix).ok())
            .map(|relative| root.join(relative))
            .collect(),
    )
}

// Returns None for anything this reader can't fully account for (split or
// sparse indexes, sha256 repos), so the caller falls back to walking the tree.
#[expect(clippy::big_endian_bytes, reason = "git index fields are big-endian")]
fn parse_index(data: &[u8]) -> Option<Vec<PathBuf>> {
    let be32 = |at: usize| -> Option<u32> {
        Some(u32::from_be_bytes(
            data.get(at..at.checked_add(4)?)?.try_into().ok()?,
        ))
    };
    let body_len = data.len().checked_sub(20)?;

    if data.get(..4)? != b"DIRC" {
        return None;
    }

    let version = be32(4)?;
    if !matches!(version, 2..=4) {
        return None;
    }

    let mut paths = Vec::new();
    let mut previous = Vec::new();
    let mut pos = 12;
    for _ in 0..be32(8)? {
        let start = pos;
        let mode = be32(start + 24)?;
        let flags = u16::from_be_bytes(data.get(start + 60..start + 62)?.try_into().ok()?);
        pos = start + 62 + if flags & 0x4000 == 0 { 0 } else { 2 };

        if version == 4 {
            let mut strip = 0;
            loop {
                let byte = *data.get(pos)?;
                pos += 1;
                strip = (strip << 7_u32) | usize::from(byte & 0x7F);
                if byte & 0x80 == 0 {
                    break;
                }
                strip += 1;
            }
            previous.truncate(previous.len().checked_sub(strip)?);
        } else {
            previous.clear();
        }

        let name_len = data
            .get(pos..body_len)?
            .iter()
            .position(|&byte| byte == 0)?;
        previous.extend_from_slice(&data[pos..pos + name_len]);
        pos += name_len + 1;
        if version != 4 {
            pos = start + (pos - 1 - start) / 8 * 8 + 8;
        }

        match mode >> 12_u32 {
            0o10 | 0o12 => paths.push(PathBuf::from(OsStr::from_bytes(&previous))),
            0o16 => {},
            _ => return None,
        }
    }

    while pos < body_len {
        if data.get(pos..pos + 4)? == b"link" {
            return None;
        }
        pos = pos.checked_add(8)?.checked_add(be32(pos + 4)? as usize)?;
    }

    (pos == body_len).then_some(paths)
}

fn walk_flake_dir(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };

        if file_type.is_dir() {
            if !FLAKE_WATCH_EXCLUDED_DIRS.contains(&name) {
                walk_flake_dir(&entry.path(), out);
            }
        } else if is_flake_input_file(name) {
            out.push(entry.path());
        }
    }
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
    #[expect(clippy::big_endian_bytes, reason = "git index fields are big-endian")]
    fn flake_watch_reads_tracked_files_from_git_index() {
        let repo = temp_dir().join(format!(
            "cade-flake-watch-index-{}-{}",
            id(),
            current().name().unwrap_or("test")
        ));
        let root = repo.join("app");
        create_dir_all(repo.join(".git")).unwrap();
        create_dir_all(root.join("nix")).unwrap();
        create_dir_all(root.join("out")).unwrap();
        write(root.join("out").join("generated.nix"), "").unwrap();

        let tracked = ["app/flake.nix", "app/nix/package.nix", "other/default.nix"];
        let mut index = b"DIRC\0\0\0\x02".to_vec();
        index.extend_from_slice(&u32::try_from(tracked.len()).unwrap().to_be_bytes());
        for path in tracked {
            let mut entry = vec![0; 62];
            entry[24..28].copy_from_slice(&0o100_644_u32.to_be_bytes());
            entry[60..62].copy_from_slice(&u16::try_from(path.len()).unwrap().to_be_bytes());
            entry.extend_from_slice(path.as_bytes());
            entry.resize((entry.len() / 8 + 1) * 8, 0);
            index.extend(entry);
        }
        index.extend([0; 20]);
        write(repo.join(".git").join("index"), index).unwrap();

        let watch = flake_watch_files(&root);

        assert!(watch.contains(&root.join("nix").join("package.nix")));
        assert!(!watch.contains(&root.join("out").join("generated.nix")));
        assert!(!watch.iter().any(|path| path.ends_with("other/default.nix")));

        let _ = remove_dir_all(&repo);
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
