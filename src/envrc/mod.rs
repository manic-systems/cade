mod directive;
mod load;
mod watch;

pub use load::load_envrc;
pub use watch::envrc_watch_files;

pub fn envrc_arg(filename: &str) -> &str {
    if filename.is_empty() {
        ".envrc"
    } else {
        filename
    }
}
