use std::iter::once;

pub(super) fn posix_single_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for ch in text.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn fish_single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

pub(super) fn posix_command(cade_exe: &str, cade_args: &[String]) -> String {
    once(cade_exe)
        .chain(cade_args.iter().map(String::as_str))
        .map(posix_single_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn fish_command(cade_exe: &str, cade_args: &[String]) -> String {
    once(cade_exe)
        .chain(cade_args.iter().map(String::as_str))
        .map(fish_single_quote)
        .collect::<Vec<_>>()
        .join(" ")
}
