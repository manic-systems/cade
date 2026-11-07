use std::{
    fs::read_to_string,
    path::{
        Path,
        PathBuf,
    },
};

use super::plan::plan_directives;

pub fn envrc_watch_files(path: &Path) -> Vec<PathBuf> {
    read_to_string(path).map_or_else(
        |_| vec![path.to_path_buf()],
        |contents| envrc_watch_files_from_contents(path, &contents),
    )
}

pub fn envrc_watch_files_from_contents(path: &Path, contents: &str) -> Vec<PathBuf> {
    let dir = path.parent().unwrap_or(path);
    let mut files = vec![path.to_path_buf()];
    for directive in plan_directives(dir, contents) {
        files.extend(directive.watch);
    }
    files
}
