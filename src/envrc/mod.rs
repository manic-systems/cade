mod directive;
pub mod load;
mod plan;
pub mod watch;

pub const fn envrc_arg(filename: &str) -> &str {
    if filename.is_empty() {
        ".envrc"
    } else {
        filename
    }
}
