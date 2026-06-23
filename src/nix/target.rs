use crate::types::LoadSpec;
use std::path::{Path, PathBuf};

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
}
