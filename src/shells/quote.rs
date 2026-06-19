pub(super) fn posix_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

fn fish_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

pub(super) fn posix_command(cade_exe: &str, cade_args: &[String]) -> String {
    std::iter::once(cade_exe)
        .chain(cade_args.iter().map(String::as_str))
        .map(posix_single_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn fish_command(cade_exe: &str, cade_args: &[String]) -> String {
    std::iter::once(cade_exe)
        .chain(cade_args.iter().map(String::as_str))
        .map(fish_single_quote)
        .collect::<Vec<_>>()
        .join(" ")
}
