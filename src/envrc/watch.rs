use std::{
    fs::read_to_string,
    path::{
        Path,
        PathBuf,
    },
};

use super::plan::plan_directives;

pub fn envrc_watch_files(path: &Path) -> Vec<PathBuf> {
    let dir = path.parent().unwrap_or(path);
    let mut files = vec![path.to_path_buf()];
    let Ok(contents) = read_to_string(path) else {
        return files;
    };
    for directive in plan_directives(dir, &contents) {
        files.extend(directive.watch);
    }
    files
}
