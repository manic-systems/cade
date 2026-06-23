use crate::loaders::load_env;
use crate::nix::{FlakeTarget, load_flake, load_shell};
use crate::{
    env::EnvSet,
    verbosity::{self, Verbosity},
};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
enum Directive {
    UseFlake(Option<String>),
    UseNix(String),
    Dotenv { file: String, if_exists: bool },
    Export(String, String),
    PathAdd(Vec<String>),
    WatchFile(Vec<String>),
    Unhandled(String),
}

fn is_literal_value(v: &str) -> bool {
    !v.contains('$') && !v.contains('`')
}

fn parse_line(raw: &str) -> Option<Directive> {
    let line = raw.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let Some(tokens) = shlex::split(line) else {
        return Some(Directive::Unhandled(line.to_string()));
    };
    let (cmd, rest) = tokens.split_first()?;
    let unhandled = || Some(Directive::Unhandled(line.to_string()));

    match cmd.as_str() {
        "use" => match rest.first().map(String::as_str) {
            Some("flake") => {
                let args = &rest[1..];
                let positional: Vec<&String> =
                    args.iter().filter(|a| !a.starts_with('-')).collect();
                let has_flags = args.iter().any(|a| a.starts_with('-'));
                // unsupported flags or multiple installables
                if has_flags || positional.len() > 1 {
                    return unhandled();
                }
                match positional.first().map(|s| s.as_str()) {
                    None | Some(".") => Some(Directive::UseFlake(None)),
                    Some(s) if s.starts_with(".#") => {
                        Some(Directive::UseFlake(Some(s[2..].to_string())))
                    }
                    // load_flake needs local relative refs
                    Some(_) => unhandled(),
                }
            }
            Some("nix") => {
                let args = &rest[1..];
                if args.iter().any(|a| a.starts_with('-')) {
                    return unhandled();
                }
                Some(Directive::UseNix(args.first().cloned().unwrap_or_default()))
            }
            _ => unhandled(),
        },
        "dotenv" => Some(Directive::Dotenv {
            file: rest.first().cloned().unwrap_or_default(),
            if_exists: false,
        }),
        "dotenv_if_exists" => Some(Directive::Dotenv {
            file: rest.first().cloned().unwrap_or_default(),
            if_exists: true,
        }),
        "PATH_add" if !rest.is_empty() => Some(Directive::PathAdd(rest.to_vec())),
        "watch_file" if !rest.is_empty() => Some(Directive::WatchFile(rest.to_vec())),
        "export" => match rest.first().and_then(|t| t.split_once('=')) {
            Some((k, v)) if is_literal_value(v) && crate::shells::is_valid_key(k) => {
                Some(Directive::Export(k.to_string(), v.to_string()))
            }
            _ => unhandled(),
        },
        _ => unhandled(),
    }
}

fn parse(contents: &str) -> Vec<Directive> {
    contents.lines().filter_map(parse_line).collect()
}

fn merge(out: &mut EnvSet, other: EnvSet) {
    out.merge_loaded(other);
}

pub fn envrc_arg(filename: &str) -> &str {
    if filename.is_empty() {
        ".envrc"
    } else {
        filename
    }
}

pub fn load_envrc(path: &Path, profile_dir: Option<PathBuf>) -> Result<EnvSet> {
    let dir = path.parent().unwrap_or(path);
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("reading .envrc at {}", path.display()))?;

    let mut out = EnvSet::new();

    let mut warnings = Vec::new();
    for (idx, directive) in parse(&contents).into_iter().enumerate() {
        match directive {
            Directive::UseFlake(output) => {
                let profile = profile_dir
                    .as_ref()
                    .map(|base| base.join(format!("{idx}-flake")));
                let target = FlakeTarget::bare_output(dir, output.as_deref());
                merge(&mut out, load_flake(&target, profile).context("use flake")?)
            }
            Directive::UseNix(file) => {
                let profile = profile_dir
                    .as_ref()
                    .map(|base| base.join(format!("{idx}-nix")));
                let shell = dir.join(if file.is_empty() { "shell.nix" } else { &file });
                merge(&mut out, load_shell(&shell, profile).context("use nix")?)
            }
            Directive::Dotenv { file, if_exists } => {
                let p = dir.join(if file.is_empty() { ".env" } else { &file });
                if if_exists && !p.exists() {
                    continue;
                }
                merge(&mut out, load_env(&p).context("dotenv")?);
            }
            Directive::Export(key, value) => {
                out.add_literal_export(key, &value);
            }
            Directive::PathAdd(dirs) => {
                let prefix: Vec<String> = dirs
                    .iter()
                    .map(|d| dir.join(d).to_string_lossy().into_owned())
                    .collect();
                out.prepend_path_entries(prefix);
            }
            Directive::WatchFile(_) => {}
            Directive::Unhandled(line) => warnings.push(line),
        }
    }

    if !warnings.is_empty() && verbosity::enabled(Verbosity::Normal) {
        verbosity::log(
            Verbosity::Normal,
            format_args!(
                "cade: ignored {} unsupported line(s) in {} (not executed):",
                warnings.len(),
                path.display()
            ),
        );
        for line in &warnings {
            verbosity::log(Verbosity::Normal, format_args!("    {line}"));
        }
    }

    Ok(out)
}

pub fn envrc_watch_files(path: &Path) -> Vec<PathBuf> {
    let dir = path.parent().unwrap_or(path);
    let mut files = vec![path.to_path_buf()];
    let Ok(contents) = std::fs::read_to_string(path) else {
        return files;
    };
    for directive in parse(&contents) {
        match directive {
            Directive::UseFlake(_) => {
                files.push(dir.join("flake.nix"));
                files.push(dir.join("flake.lock"));
            }
            Directive::UseNix(f) => {
                files.push(dir.join(if f.is_empty() { "shell.nix" } else { &f }));
            }
            Directive::Dotenv { file, .. } => {
                files.push(dir.join(if file.is_empty() { ".env" } else { &file }));
            }
            Directive::WatchFile(ws) => files.extend(ws.iter().map(|w| dir.join(w))),
            _ => {}
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    const STORE_PATH: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-envrc";

    #[test]
    fn recognizes_declarative_directives() {
        assert_eq!(parse_line("use flake"), Some(Directive::UseFlake(None)));
        assert_eq!(parse_line("use flake ."), Some(Directive::UseFlake(None)));
        assert_eq!(
            parse_line("use flake .#dev"),
            Some(Directive::UseFlake(Some("dev".to_string())))
        );
        assert_eq!(
            parse_line("use nix shell.nix"),
            Some(Directive::UseNix("shell.nix".to_string()))
        );
        assert_eq!(
            parse_line("dotenv_if_exists .env.local"),
            Some(Directive::Dotenv {
                file: ".env.local".to_string(),
                if_exists: true
            })
        );
        assert_eq!(
            parse_line("export FOO=bar"),
            Some(Directive::Export("FOO".to_string(), "bar".to_string()))
        );
        assert_eq!(
            parse_line("PATH_add ./bin"),
            Some(Directive::PathAdd(vec!["./bin".to_string()]))
        );
    }

    #[test]
    fn use_flake_named_output_stays_bare_output() {
        // parsed outputs must not re-enter path classification
        let Some(Directive::UseFlake(output)) = parse_line("use flake .#dev") else {
            panic!("expected UseFlake");
        };
        let target = crate::nix::FlakeTarget::bare_output(Path::new("/layer"), output.as_deref());
        assert_eq!(target.installable, ".#dev");
        assert_eq!(target.spec.cache_key(), "flake:dev");
        assert_eq!(target.cwd, Path::new("/layer"));
    }

    #[test]
    fn literal_export_records_store_paths() {
        let dir =
            std::env::temp_dir().join(format!("cade-envrc-store-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".envrc");
        std::fs::write(&path, format!("export TOOL={STORE_PATH}\n")).unwrap();

        let env = load_envrc(&path, None).unwrap();

        assert_eq!(env.derived_store_paths(), [STORE_PATH]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn comments_and_blanks_are_ignored() {
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("   # a comment"), None);
        assert_eq!(parse_line("#!/usr/bin/env bash"), None);
    }

    #[test]
    fn unmappable_lines_are_flagged_not_dropped() {
        assert!(matches!(
            parse_line("export PATH=$PATH:./bin"),
            Some(Directive::Unhandled(_))
        ));
        assert!(matches!(
            parse_line("export X=$(date)"),
            Some(Directive::Unhandled(_))
        ));
        assert!(matches!(
            parse_line("use flake . --impure"),
            Some(Directive::Unhandled(_))
        ));
        assert!(matches!(
            parse_line("use flake github:foo/bar"),
            Some(Directive::Unhandled(_))
        ));
        assert!(matches!(
            parse_line("layout python"),
            Some(Directive::Unhandled(_))
        ));
        assert!(matches!(
            parse_line("source_up"),
            Some(Directive::Unhandled(_))
        ));
    }
}
