use crate::{config, types::Keyword};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DirKind {
    Cade,
    Envrc,
}

fn dir_kind(dir: &Path) -> Option<DirKind> {
    if std::fs::exists(dir.join(".cade")).unwrap_or(false) {
        Some(DirKind::Cade)
    } else if config::direnv_mode().loads_envrc()
        && std::fs::exists(dir.join(".envrc")).unwrap_or(false)
    {
        Some(DirKind::Envrc)
    } else {
        None
    }
}

fn caps_the_cascade(dir: &Path) -> bool {
    match crate::cade_file::read(&dir.join(".cade")) {
        Ok(kws) => kws.iter().any(|kw| matches!(kw, Keyword::Disinherit)),
        Err(_) => true,
    }
}

pub(super) fn participant_dirs(start: &Path) -> Vec<PathBuf> {
    let mut cade_chain: Vec<PathBuf> = Vec::new();
    let mut nearest_envrc: Option<PathBuf> = None;

    let mut dir = Some(start.to_path_buf());
    while let Some(d) = dir {
        match dir_kind(&d) {
            Some(DirKind::Cade) => {
                cade_chain.push(d.clone());
                if caps_the_cascade(&d) {
                    break;
                }
            }
            Some(DirKind::Envrc) => {
                nearest_envrc.get_or_insert_with(|| d.clone());
            }
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
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    dirs
}

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

        assert_eq!(find_cade_root(&nested), Some(base.join("a")));
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
        assert_participants(&[("a", ".cade"), ("a/b", ".cade")], "a/b", &["a/b", "a"]);
    }

    #[test]
    fn participants_nearest_envrc_only_no_cade() {
        assert_participants(&[("a", ".envrc"), ("a/b", ".envrc")], "a/b", &["a/b"]);
    }

    #[test]
    fn participants_cade_union_nearest_envrc_below() {
        assert_participants(&[("a", ".cade"), ("a/b", ".envrc")], "a/b", &["a/b", "a"]);
    }

    #[test]
    fn participants_cade_union_nearest_envrc_above() {
        assert_participants(&[("a", ".envrc"), ("a/b", ".cade")], "a/b", &["a/b", "a"]);
    }

    #[test]
    fn participants_only_nearest_envrc_enters_with_a_gap() {
        assert_participants(
            &[("a", ".cade"), ("a/b", ".envrc"), ("a/b/c", ".envrc")],
            "a/b/c",
            &["a/b/c", "a"],
        );
    }

    #[test]
    fn participants_cade_cascade_spans_a_gap() {
        assert_participants(
            &[("a", ".cade"), ("a/b/c", ".cade")],
            "a/b/c",
            &["a/b/c", "a"],
        );
    }

    #[test]
    fn participants_upper_envrc_survives_a_cade_cascade_gap() {
        assert_participants(
            &[("a", ".envrc"), ("a/b/c", ".cade")],
            "a/b/c",
            &["a/b/c", "a"],
        );
    }

    #[test]
    fn participants_colocated_envrc_is_ignored() {
        let base = std::env::temp_dir().join(format!("cade-parts-both-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        let a = base.join("a");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join(".cade"), b"").unwrap();
        std::fs::write(a.join(".envrc"), b"").unwrap();
        assert_eq!(parts(&participant_dirs(&a), &base), vec!["a".to_string()]);
        std::fs::remove_dir_all(&base).ok();
    }

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

    #[test]
    fn malformed_cade_caps_the_cascade_instead_of_being_skipped() {
        let base = build_tree(
            &[
                ("a", ".cade", "A_CADE=1\n"),
                ("a/b", ".cade", "not a keyword\n"),
            ],
            "malformed-caps-midchain",
        );
        let cwd = base.join("a/b");
        assert_eq!(
            parts(&participant_dirs(&cwd), &base),
            vec!["a/b".to_string()],
            "malformed .cade must cap the cascade, not skip up to the parent"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn malformed_cade_caps_even_with_a_deeper_valid_tip() {
        let base = build_tree(
            &[
                ("a", ".cade", "A_CADE=1\n"),
                ("a/b", ".cade", "not a keyword\n"),
                ("a/b/tip", ".cade", "TIP_CADE=1\n"),
            ],
            "malformed-caps-with-tip",
        );
        let cwd = base.join("a/b/tip");
        assert_eq!(
            parts(&participant_dirs(&cwd), &base),
            vec!["a/b/tip".to_string(), "a/b".to_string()],
            "the malformed dir caps the chain; the valid grandparent must not join"
        );
        std::fs::remove_dir_all(&base).ok();
    }
}
