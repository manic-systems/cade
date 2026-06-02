//! Pure path topology for the cascade: which directories participate in an
//! activation at a given cwd, and which one is the root. No db, no env, no
//! shell output - just the filesystem layout of `.cade` and `.envrc` markers.

use crate::types::Keyword;
use std::path::{Path, PathBuf};

/// what a dir contributes; a co-located `.envrc` yields to `.cade`, so at most one kind
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DirKind {
    Cade,
    Envrc,
}

fn dir_kind(dir: &Path) -> Option<DirKind> {
    if std::fs::exists(dir.join(".cade")).unwrap_or(false) {
        Some(DirKind::Cade)
    } else if std::fs::exists(dir.join(".envrc")).unwrap_or(false) {
        Some(DirKind::Envrc)
    } else {
        None
    }
}

/// true when `dir`'s `.cade` carries a `disinherit` directive (caps the cascade)
fn reads_disinherit(dir: &Path) -> bool {
    matches!(
        super::read_cade(&dir.join(".cade")),
        Ok(kws) if kws.iter().any(|kw| matches!(kw, Keyword::Disinherit))
    )
}

/// the active layer set, tip-first: every `.cade` ancestor (the cascade stacks
/// across gaps; an empty intermediate dir does not sever it) unioned with
/// direnv's single nearest `.envrc`. `disinherit` halts the cascade; otherwise
/// only the permission layer caps it
//
// note: a `disinherit` dir is parsed here and re-parsed at activation via
// `config_keywords`; a single-parse pass shared across both is a deferred
// cross-cutting refactor (touches the composition-branch callers)
pub(super) fn participant_dirs(start: &Path) -> Vec<PathBuf> {
    let mut cade_chain: Vec<PathBuf> = Vec::new();
    let mut nearest_envrc: Option<PathBuf> = None;

    let mut dir = Some(start.to_path_buf());
    while let Some(d) = dir {
        match dir_kind(&d) {
            Some(DirKind::Cade) => {
                // include this dir, then stop the cascade if it disinherits
                cade_chain.push(d.clone());
                if reads_disinherit(&d) {
                    break;
                }
            }
            Some(DirKind::Envrc) => {
                // only the nearest .envrc
                nearest_envrc.get_or_insert_with(|| d.clone());
            }
            // a gap does not break the cascade
            None => {}
        }
        dir = d.parent().map(Path::to_path_buf);
    }

    merge_participants(cade_chain, nearest_envrc)
}

fn merge_participants(cade_chain: Vec<PathBuf>, nearest_envrc: Option<PathBuf>) -> Vec<PathBuf> {
    let mut dirs = cade_chain;
    if let Some(envrc) = nearest_envrc
        && !dirs.contains(&envrc)
    {
        dirs.push(envrc);
    }
    // deepest first
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    dirs
}

/// activation root: the deepest participant (may be an `.envrc` below the nearest `.cade`)
//
// This re-walks and sorts the whole participant set just to take the deepest.
// A bespoke early-returning walk would shave that off the activation hot-path,
// but the set is a handful of ancestors and a second walk would have to
// re-derive the same .cade/.envrc union rules, inviting the very drift the
// shared `participant_dirs` exists to prevent. The cost is accepted.
pub(super) fn find_cade_root(start: &Path) -> Option<PathBuf> {
    participant_dirs(start).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_cade_root_walks_up_to_innermost() {
        let base = std::env::temp_dir().join(format!("cade-root-{}", std::process::id()));
        let nested = base.join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(base.join("a").join(".cade"), b"").unwrap();

        // from c (no .cade), the innermost ancestor with .cade is a/
        assert_eq!(find_cade_root(&nested), Some(base.join("a")));
        // adding a deeper .cade changes the root
        std::fs::write(base.join("a/b").join(".cade"), b"").unwrap();
        assert_eq!(find_cade_root(&nested), Some(base.join("a/b")));

        std::fs::remove_dir_all(&base).ok();
    }

    fn parts(dirs: &[PathBuf], base: &Path) -> Vec<String> {
        dirs.iter()
            .map(|d| {
                d.strip_prefix(base)
                    .unwrap_or(d)
                    .to_string_lossy()
                    .to_string()
            })
            .collect()
    }

    /// build a temp tree per spec, then assert the tip-first participant list
    fn assert_participants(spec: &[(&str, &str)], cwd_rel: &str, expect_tip_first: &[&str]) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SALT: AtomicU32 = AtomicU32::new(0);
        let base = std::env::temp_dir().join(format!(
            "cade-parts-{}-{}",
            std::process::id(),
            SALT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::remove_dir_all(&base).ok();
        for (rel, file) in spec {
            let dir = base.join(rel);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(file), b"").unwrap();
        }
        let cwd = base.join(cwd_rel);
        std::fs::create_dir_all(&cwd).unwrap();
        let got = parts(&participant_dirs(&cwd), &base);
        let want: Vec<String> = expect_tip_first.iter().map(|s| s.to_string()).collect();
        assert_eq!(got, want, "spec {spec:?} cwd {cwd_rel}");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn participants_cade_cascade() {
        // S1: contiguous .cade cascade, tip-first
        assert_participants(&[("a", ".cade"), ("a/b", ".cade")], "a/b", &["a/b", "a"]);
    }

    #[test]
    fn participants_nearest_envrc_only_no_cade() {
        // S2: stacked .envrc, no .cade -> only the nearest one
        assert_participants(&[("a", ".envrc"), ("a/b", ".envrc")], "a/b", &["a/b"]);
    }

    #[test]
    fn participants_cade_union_nearest_envrc_below() {
        // S3: cade {a} union nearest envrc {a/b}
        assert_participants(&[("a", ".cade"), ("a/b", ".envrc")], "a/b", &["a/b", "a"]);
    }

    #[test]
    fn participants_cade_union_nearest_envrc_above() {
        // S4: cade {a/b} union nearest envrc {a}
        assert_participants(&[("a", ".envrc"), ("a/b", ".cade")], "a/b", &["a/b", "a"]);
    }

    #[test]
    fn participants_only_nearest_envrc_enters_with_a_gap() {
        // S5: cade {a} union nearest envrc {a/b/c}; a/b is dropped (a hole)
        assert_participants(
            &[("a", ".cade"), ("a/b", ".envrc"), ("a/b/c", ".envrc")],
            "a/b/c",
            &["a/b/c", "a"],
        );
    }

    #[test]
    fn participants_cade_cascade_spans_a_gap() {
        // an empty intermediate dir does not sever the cascade
        assert_participants(
            &[("a", ".cade"), ("a/b/c", ".cade")],
            "a/b/c",
            &["a/b/c", "a"],
        );
    }

    #[test]
    fn participants_upper_envrc_survives_a_cade_cascade_gap() {
        // the gap at b excludes an upper .cade, but the nearest .envrc above it still joins
        assert_participants(
            &[("a", ".envrc"), ("a/b/c", ".cade")],
            "a/b/c",
            &["a/b/c", "a"],
        );
    }

    #[test]
    fn participants_colocated_envrc_is_ignored() {
        // S6: a dir with both is a .cade layer; its .envrc never participates
        let base = std::env::temp_dir().join(format!("cade-parts-both-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        let a = base.join("a");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join(".cade"), b"").unwrap();
        std::fs::write(a.join(".envrc"), b"").unwrap();
        assert_eq!(parts(&participant_dirs(&a), &base), vec!["a".to_string()]);
        std::fs::remove_dir_all(&base).ok();
    }

    /// Build a temp tree from (rel-dir, filename, contents) entries.
    fn build_tree(spec: &[(&str, &str, &str)], tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SALT: AtomicU32 = AtomicU32::new(0);
        let base = std::env::temp_dir().join(format!(
            "cade-{tag}-{}-{}",
            std::process::id(),
            SALT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::remove_dir_all(&base).ok();
        for (rel, file, contents) in spec {
            let dir = base.join(rel);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(file), contents.as_bytes()).unwrap();
        }
        base
    }

    #[test]
    fn disinherit_truncates_the_cade_cascade() {
        // child .cade disinherits, so its .cade parent never joins the chain.
        let base = build_tree(
            &[("a", ".cade", ""), ("a/b", ".cade", "disinherit\n")],
            "disinherit",
        );
        let cwd = base.join("a/b");
        assert_eq!(
            parts(&participant_dirs(&cwd), &base),
            vec!["a/b".to_string()]
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn disinherit_still_unions_the_nearest_envrc() {
        // disinherit drops the parent .cade, but a nearer .envrc still composes.
        let base = build_tree(
            &[
                ("a", ".cade", ""),
                ("a/b", ".cade", "disinherit\n"),
                ("a/b/c", ".envrc", "export X=1\n"),
            ],
            "disinherit-envrc",
        );
        let cwd = base.join("a/b/c");
        assert_eq!(
            parts(&participant_dirs(&cwd), &base),
            vec!["a/b/c".to_string(), "a/b".to_string()]
        );
        std::fs::remove_dir_all(&base).ok();
    }
}
