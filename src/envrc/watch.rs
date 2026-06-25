use super::directive::{Directive, parse};
use std::path::{Path, PathBuf};

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
