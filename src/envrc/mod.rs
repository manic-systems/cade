mod directive;
pub mod load;
mod plan;
pub mod watch;

use std::{
    fs::read_to_string,
    path::Path,
};

use directive::{
    Directive,
    parse,
};

pub const fn envrc_arg(filename: &str) -> &str {
    if filename.is_empty() {
        ".envrc"
    } else {
        filename
    }
}

pub fn envrc_uses_nix(path: &Path) -> bool {
    read_to_string(path).is_ok_and(|contents| {
        parse(&contents)
            .iter()
            .any(|directive| matches!(*directive, Directive::UseFlake(_) | Directive::UseNix(_)))
    })
}
