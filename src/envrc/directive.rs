use crate::shells::is_valid_key;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Directive {
    UseFlake(Option<String>),
    UseNix(String),
    Dotenv { file: String, if_exists: bool },
    Export(String, String),
    PathAdd(Vec<String>),
    WatchFile(Vec<String>),
    Unhandled(String),
}

pub(super) fn parse(contents: &str) -> Vec<Directive> {
    contents.lines().filter_map(parse_line).collect()
}

fn is_literal_value(value: &str) -> bool {
    !value.contains('$') && !value.contains('`')
}

fn parse_line(raw: &str) -> Option<Directive> {
    let line = raw.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let Some(tokens) = shlex::split(line) else {
        return Some(Directive::Unhandled(line.to_owned()));
    };
    let (cmd, rest) = tokens.split_first()?;
    let unhandled = || Some(Directive::Unhandled(line.to_owned()));

    match cmd.as_str() {
        "use" => match rest.first().map(String::as_str) {
            Some("flake") => parse_use_flake(rest, unhandled),
            Some("nix") => parse_use_nix(rest, unhandled),
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
        "export" => match rest
            .first()
            .and_then(|assignment| assignment.split_once('='))
        {
            Some((key, val)) if is_literal_value(val) && is_valid_key(key) => {
                Some(Directive::Export(key.to_owned(), val.to_owned()))
            }
            _ => unhandled(),
        },
        _ => unhandled(),
    }
}

fn parse_use_flake<Fallback>(rest: &[String], unhandled: Fallback) -> Option<Directive>
where
    Fallback: FnOnce() -> Option<Directive>,
{
    let args = &rest[1..];
    let positional: Vec<&String> = args.iter().filter(|arg| !arg.starts_with('-')).collect();
    let has_flags = args.iter().any(|arg| arg.starts_with('-'));
    if has_flags || positional.len() > 1 {
        return unhandled();
    }
    match positional.first().map(|entry| entry.as_str()) {
        None | Some(".") => Some(Directive::UseFlake(None)),
        Some(spec) => spec.strip_prefix(".#").map_or_else(unhandled, |name| {
            Some(Directive::UseFlake(Some(name.to_owned())))
        }),
    }
}

fn parse_use_nix<Fallback>(rest: &[String], unhandled: Fallback) -> Option<Directive>
where
    Fallback: FnOnce() -> Option<Directive>,
{
    let args = &rest[1..];
    if args.iter().any(|arg| arg.starts_with('-')) {
        return unhandled();
    }
    Some(Directive::UseNix(args.first().cloned().unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nix::target::FlakeTarget;
    use std::path::Path;

    #[test]
    fn recognizes_declarative_directives() {
        assert_eq!(parse_line("use flake"), Some(Directive::UseFlake(None)));
        assert_eq!(parse_line("use flake ."), Some(Directive::UseFlake(None)));
        assert_eq!(
            parse_line("use flake .#dev"),
            Some(Directive::UseFlake(Some("dev".to_owned())))
        );
        assert_eq!(
            parse_line("use nix shell.nix"),
            Some(Directive::UseNix("shell.nix".to_owned()))
        );
        assert_eq!(
            parse_line("dotenv_if_exists .env.local"),
            Some(Directive::Dotenv {
                file: ".env.local".to_owned(),
                if_exists: true
            })
        );
        assert_eq!(
            parse_line("export FOO=bar"),
            Some(Directive::Export("FOO".to_owned(), "bar".to_owned()))
        );
        assert_eq!(
            parse_line("PATH_add ./bin"),
            Some(Directive::PathAdd(vec!["./bin".to_owned()]))
        );
    }

    #[test]
    fn use_flake_named_output_stays_bare_output() {
        assert_eq!(
            parse_line("use flake .#dev"),
            Some(Directive::UseFlake(Some("dev".to_owned())))
        );
        let target = FlakeTarget::bare_output(Path::new("/layer"), Some("dev"));
        assert_eq!(target.installable, ".#dev");
        assert_eq!(target.spec.cache_key(), "flake:dev");
        assert_eq!(target.cwd, Path::new("/layer"));
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
