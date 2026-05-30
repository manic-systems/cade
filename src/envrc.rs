//! Compatibility shim for direnv `.envrc` files.
//!
//! cade does not execute `.envrc` as a shell script. Instead it recognizes the
//! declarative subset of the direnv stdlib that maps cleanly onto cade's own
//! loaders (`use flake`, `use nix`, `dotenv`, `watch_file`, plus literal
//! `export`/`PATH_add`) and warns about any line it can't faithfully
//! reproduce. This should cover most cases of .envrc.

use crate::loaders::{load_env, load_flake, load_shell};
use crate::{
    types::EnvSet,
    verbosity::{self, Verbosity},
};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
enum Directive {
    /// `use flake` / `use flake .` / `use flake .#output`
    UseFlake(Option<String>),
    /// `use nix [file]`
    UseNix(String),
    /// `dotenv [file]` / `dotenv_if_exists [file]`
    Dotenv { file: String, if_exists: bool },
    /// `export KEY=VALUE` with a literal (unexpanded) value
    Export(String, String),
    /// `PATH_add DIR...`: directories to prepend to PATH
    PathAdd(Vec<String>),
    /// `watch_file FILE...`
    WatchFile(Vec<String>),
    /// A line cade can't faithfully map; carried so we can warn about it.
    Unhandled(String),
}

/// Ensure no shell expansion is taking place
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
                // don't honor flags or multiple installables
                if has_flags || positional.len() > 1 {
                    return unhandled();
                }
                match positional.first().map(|s| s.as_str()) {
                    None | Some(".") => Some(Directive::UseFlake(None)),
                    Some(s) if s.starts_with(".#") => {
                        Some(Directive::UseFlake(Some(s[2..].to_string())))
                    }
                    // remote refs / absolute paths aren't supported by load_flake
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

/// Merge another env set in, extending list values and carrying over hard-replaces
fn merge(out: &mut EnvSet, other: EnvSet) {
    for (k, v) in other.vars {
        out.vars
            .entry(k)
            .and_modify(|cur: &mut Vec<String>| cur.extend(v.clone()))
            .or_insert(v);
    }
    out.hard.extend(other.hard);
    out.nix_store_paths.extend(other.nix_store_paths);
}

fn envrc_path(dir: &Path, filename: &str) -> PathBuf {
    dir.join(if filename.is_empty() {
        ".envrc"
    } else {
        filename
    })
}

/// Compose an .envrc's recognized directives into a single EnvSet
pub fn load_envrc(dir: &Path, filename: String) -> Result<EnvSet> {
    let path = envrc_path(dir, &filename);
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("reading .envrc at {}", path.display()))?;

    let mut out = EnvSet::new();

    let mut warnings = Vec::new();
    for directive in parse(&contents) {
        match directive {
            Directive::UseFlake(output) => {
                merge(&mut out, load_flake(dir, output).context("use flake")?)
            }
            Directive::UseNix(file) => merge(&mut out, load_shell(dir, file).context("use nix")?),
            Directive::Dotenv { file, if_exists } => {
                let p = if file.is_empty() {
                    dir.join(".env")
                } else {
                    dir.join(&file)
                };
                if if_exists && !p.exists() {
                    continue;
                }
                merge(&mut out, load_env(dir, file).context("dotenv")?);
            }
            Directive::Export(key, value) => {
                let parts: Vec<String> = value.split(':').map(str::to_string).collect();
                out.vars
                    .entry(key)
                    .and_modify(|cur| cur.extend(parts.clone()))
                    .or_insert(parts);
            }
            Directive::PathAdd(dirs) => {
                // direnv PATH_add prepends, resolved against the .envrc's dir
                let mut prefix: Vec<String> = dirs
                    .iter()
                    .map(|d| dir.join(d).to_string_lossy().into_owned())
                    .collect();
                let entry = out.vars.entry("PATH".to_string()).or_default();
                prefix.append(entry);
                *entry = prefix;
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

/// Files an .envrc layer depends on
pub fn envrc_watch_files(dir: &Path, filename: String) -> Vec<PathBuf> {
    let path = envrc_path(dir, &filename);
    let mut files = vec![path.clone()];
    let Ok(contents) = std::fs::read_to_string(&path) else {
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
    fn comments_and_blanks_are_ignored() {
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("   # a comment"), None);
        assert_eq!(parse_line("#!/usr/bin/env bash"), None);
    }

    #[test]
    fn unmappable_lines_are_flagged_not_dropped() {
        // expansion in a value can't be reproduced literally
        assert!(matches!(
            parse_line("export PATH=$PATH:./bin"),
            Some(Directive::Unhandled(_))
        ));
        assert!(matches!(
            parse_line("export X=$(date)"),
            Some(Directive::Unhandled(_))
        ));
        // flags we can't honor
        assert!(matches!(
            parse_line("use flake . --impure"),
            Some(Directive::Unhandled(_))
        ));
        // remote flake refs aren't supported by the loader
        assert!(matches!(
            parse_line("use flake github:foo/bar"),
            Some(Directive::Unhandled(_))
        ));
        // unknown stdlib functions
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
