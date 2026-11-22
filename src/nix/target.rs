use std::path::{
    Path,
    PathBuf,
};

use crate::{
    path_resolve::resolve_for_watch,
    types::load_spec::LoadSpec,
};

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

// Matches nix-direnv. Anything else the dev shell reads needs a `watch`.
pub fn flake_watch_files(root: &Path) -> Vec<PathBuf> {
    vec![root.join("flake.lock"), root.join("flake.nix")]
}

#[cfg(test)]
mod tests {
    use std::{
        env::temp_dir,
        fs::{
            canonicalize,
            create_dir_all,
            remove_dir_all,
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
        assert_eq!(target.installable, "");
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
}
