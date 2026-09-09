use crate::{cade_file::read, config, types::keyword::Keyword};
use std::fs::exists;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DirKind {
    Cade,
    Envrc,
}

fn dir_kind(dir: &Path) -> Option<DirKind> {
    if exists(dir.join(".cade")).unwrap_or(false) {
        Some(DirKind::Cade)
    } else if config::direnv_mode().loads_envrc() && exists(dir.join(".envrc")).unwrap_or(false) {
        Some(DirKind::Envrc)
    } else {
        None
    }
}

fn caps_the_cascade(dir: &Path) -> bool {
    read(&dir.join(".cade")).map_or(true, |kws| {
        kws.iter().any(|kw| matches!(kw, Keyword::Disinherit))
    })
}

pub(super) fn participant_dirs(start: &Path) -> Vec<PathBuf> {
    let mut cade_chain: Vec<PathBuf> = Vec::new();
    let mut nearest_envrc: Option<PathBuf> = None;

    let mut dir = Some(start.to_path_buf());
    while let Some(current) = dir {
        match dir_kind(&current) {
            Some(DirKind::Cade) => {
                cade_chain.push(current.clone());
                if caps_the_cascade(&current) {
                    break;
                }
            }
            Some(DirKind::Envrc) => {
                nearest_envrc.get_or_insert_with(|| current.clone());
            }
            None => {}
        }
        dir = current.parent().map(Path::to_path_buf);
    }

    merge_participants(cade_chain, nearest_envrc)
}

fn merge_participants(cade_chain: Vec<PathBuf>, nearest_envrc: Option<PathBuf>) -> Vec<PathBuf> {
    let mut dirs = cade_chain;
    if let Some(envrc) = nearest_envrc
        && !dirs.contains(&envrc)
    {
        let index = dirs.partition_point(|dir| dir.starts_with(&envrc));
        dirs.insert(index, envrc);
    }
    dirs
}

pub(super) fn find_cade_root(start: &Path) -> Option<PathBuf> {
    participant_dirs(start).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;
    use std::fs::{create_dir_all, remove_dir_all, write};
    use std::process::id;

    #[test]
    fn find_cade_root_walks_up_to_innermost() {
        let base = temp_dir().join(format!("cade-root-{}", id()));
        let nested = base.join("a/b/c");
        create_dir_all(&nested).unwrap();
        write(base.join("a").join(".cade"), b"").unwrap();

        assert_eq!(find_cade_root(&nested), Some(base.join("a")));
        write(base.join("a/b").join(".cade"), b"").unwrap();
        assert_eq!(find_cade_root(&nested), Some(base.join("a/b")));

        let _ = remove_dir_all(&base);
    }

    fn parts(dirs: &[PathBuf], base: &Path) -> Vec<String> {
        dirs.iter()
            .map(|dir| {
                dir.strip_prefix(base)
                    .unwrap_or(dir)
                    .to_string_lossy()
                    .to_string()
            })
            .collect()
    }

    fn assert_participants(spec: &[(&str, &str)], cwd_rel: &str, expect_tip_first: &[&str]) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SALT: AtomicU32 = AtomicU32::new(0);
        let base = temp_dir().join(format!(
            "cade-parts-{}-{}",
            id(),
            SALT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = remove_dir_all(&base);
        for &(rel, file) in spec {
            let dir = base.join(rel);
            create_dir_all(&dir).unwrap();
            write(dir.join(file), b"").unwrap();
        }
        let cwd = base.join(cwd_rel);
        create_dir_all(&cwd).unwrap();
        let got = parts(&participant_dirs(&cwd), &base);
        let want: Vec<String> = expect_tip_first.iter().map(ToString::to_string).collect();
        assert_eq!(got, want, "spec {spec:?} cwd {cwd_rel}");
        let _ = remove_dir_all(&base);
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
        let base = temp_dir().join(format!("cade-parts-both-{}", id()));
        let _ = remove_dir_all(&base);
        let colocated = base.join("a");
        create_dir_all(&colocated).unwrap();
        write(colocated.join(".cade"), b"").unwrap();
        write(colocated.join(".envrc"), b"").unwrap();
        assert_eq!(
            parts(&participant_dirs(&colocated), &base),
            vec!["a".to_owned()]
        );
        let _ = remove_dir_all(&base);
    }

    fn build_tree(spec: &[(&str, &str, &str)], tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SALT: AtomicU32 = AtomicU32::new(0);
        let base = temp_dir().join(format!(
            "cade-{tag}-{}-{}",
            id(),
            SALT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = remove_dir_all(&base);
        for &(rel, file, contents) in spec {
            let dir = base.join(rel);
            create_dir_all(&dir).unwrap();
            write(dir.join(file), contents.as_bytes()).unwrap();
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
            vec!["a/b".to_owned()]
        );
        let _ = remove_dir_all(&base);
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
            vec!["a/b/c".to_owned(), "a/b".to_owned()]
        );
        let _ = remove_dir_all(&base);
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
            vec!["a/b".to_owned()],
            "malformed .cade must cap the cascade, not skip up to the parent"
        );
        let _ = remove_dir_all(&base);
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
            vec!["a/b/tip".to_owned(), "a/b".to_owned()],
            "the malformed dir caps the chain; the valid grandparent must not join"
        );
        let _ = remove_dir_all(&base);
    }
}
