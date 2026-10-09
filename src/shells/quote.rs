use std::iter::once;

pub fn posix_single_quote(text: &str) -> String {
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

pub fn split_words(input: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        match ch {
            ' ' | '\t' | '\n' => words.extend(word.take()),
            '#' if word.is_none() => {
                for skipped in chars.by_ref() {
                    if skipped == '\n' {
                        break;
                    }
                }
            },
            '\\' => {
                match chars.next()? {
                    '\n' => {},
                    escaped => word.get_or_insert_default().push(escaped),
                }
            },
            '\'' => {
                let current = word.get_or_insert_default();
                loop {
                    match chars.next()? {
                        '\'' => break,
                        quoted => current.push(quoted),
                    }
                }
            },
            '"' => {
                let current = word.get_or_insert_default();
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => {
                            match chars.next()? {
                                '\n' => {},
                                escaped @ ('$' | '`' | '"' | '\\') => current.push(escaped),
                                other => {
                                    current.push('\\');
                                    current.push(other);
                                },
                            }
                        },
                        quoted => current.push(quoted),
                    }
                }
            },
            other => word.get_or_insert_default().push(other),
        }
    }
    words.extend(word);
    Some(words)
}
