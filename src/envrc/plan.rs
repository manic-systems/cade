use super::directive::{Directive, parse};
use crate::nix::FlakeTarget;
use std::path::{Path, PathBuf};

pub(super) enum PlannedDirective {
    UseFlake {
        target: FlakeTarget,
        profile_name: String,
    },
    UseNix {
        shell: PathBuf,
        profile_name: String,
    },
    Dotenv {
        path: PathBuf,
        if_exists: bool,
    },
    Export(String, String),
    PathAdd(Vec<String>),
    WatchOnly,
    Unhandled(String),
}

pub(super) struct EnvrcDirective {
    pub action: PlannedDirective,
    pub watch: Vec<PathBuf>,
}

pub(super) fn plan_directives(dir: &Path, contents: &str) -> Vec<EnvrcDirective> {
    parse(contents)
        .into_iter()
        .enumerate()
        .map(|(idx, directive)| plan_directive(dir, idx, directive))
        .collect()
}

fn plan_directive(dir: &Path, idx: usize, directive: Directive) -> EnvrcDirective {
    match directive {
        Directive::UseFlake(output) => {
            let target = FlakeTarget::bare_output(dir, output.as_deref());
            EnvrcDirective {
                action: PlannedDirective::UseFlake {
                    target,
                    profile_name: format!("{idx}-flake"),
                },
                watch: crate::nix::flake_watch_files(dir),
            }
        }
        Directive::UseNix(file) => {
            let shell = dir.join(if file.is_empty() { "shell.nix" } else { &file });
            EnvrcDirective {
                action: PlannedDirective::UseNix {
                    shell: shell.clone(),
                    profile_name: format!("{idx}-nix"),
                },
                watch: vec![shell],
            }
        }
        Directive::Dotenv { file, if_exists } => {
            let path = dir.join(if file.is_empty() { ".env" } else { &file });
            EnvrcDirective {
                action: PlannedDirective::Dotenv {
                    path: path.clone(),
                    if_exists,
                },
                watch: vec![path],
            }
        }
        Directive::Export(key, value) => EnvrcDirective {
            action: PlannedDirective::Export(key, value),
            watch: Vec::new(),
        },
        Directive::PathAdd(dirs) => EnvrcDirective {
            action: PlannedDirective::PathAdd(dirs),
            watch: Vec::new(),
        },
        Directive::WatchFile(files) => EnvrcDirective {
            action: PlannedDirective::WatchOnly,
            watch: files.into_iter().map(|file| dir.join(file)).collect(),
        },
        Directive::Unhandled(line) => EnvrcDirective {
            action: PlannedDirective::Unhandled(line),
            watch: Vec::new(),
        },
    }
}
