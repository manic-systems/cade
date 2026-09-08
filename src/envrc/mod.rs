mod directive;
pub(crate) mod load;
mod plan;
mod watch;

pub use watch::envrc_watch_files;

pub fn envrc_arg(filename: &str) -> &str {
    if filename.is_empty() {
        ".envrc"
    } else {
        filename
    }
}
