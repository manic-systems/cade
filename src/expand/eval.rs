use super::Lookup;

pub(super) fn expand_plain(input: &str, lookup: Lookup<'_>) -> String {
    expand_with(input, lookup, &|value| value)
}

pub(super) fn expand_with(
    input: &str,
    lookup: Lookup<'_>,
    on_value: &dyn Fn(String) -> String,
) -> String {
    let bytes = input.as_bytes();
    let mut out = String::new();
    let mut pos = 0;
    while pos < bytes.len() {
        match bytes[pos] {
            b'\\' if bytes.get(pos + 1) == Some(&b'$') => {
                out.push('$');
                pos += 2;
            },
            b'$' if bytes.get(pos + 1) == Some(&b'{') => {
                if let Some((inner, end)) = find_close(input, pos) {
                    out.push_str(&on_value(expand_ref(inner, lookup)));
                    pos = end;
                } else {
                    out.push_str("${");
                    pos += 2;
                }
            },
            _ => {
                let ch = input
                    .get(pos..)
                    .and_then(|tail| tail.chars().next())
                    .expect("byte index is always on a char boundary");
                out.push(ch);
                pos += ch.len_utf8();
            },
        }
    }
    out
}

fn find_close(text: &str, start: usize) -> Option<(&str, usize)> {
    let bytes = text.as_bytes();
    let mut depth = 1_usize;
    let mut scan = start + 2;
    while scan < bytes.len() {
        if bytes[scan] == b'$' && bytes.get(scan + 1) == Some(&b'{') {
            depth += 1;
            scan += 2;
        } else if bytes[scan] == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some((text.get(start + 2..scan)?, scan + 1));
            }
            scan += 1;
        } else {
            scan += 1;
        }
    }
    None
}

const fn is_name_byte(byte: u8, first: bool) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || (!first && byte.is_ascii_digit())
}

fn expand_ref(inner: &str, lookup: Lookup<'_>) -> String {
    let bytes = inner.as_bytes();
    let mut name_len = 0;
    while name_len < bytes.len() && is_name_byte(bytes[name_len], name_len == 0) {
        name_len += 1;
    }
    if name_len == 0 {
        return format!("${{{inner}}}");
    }
    let name = inner
        .get(..name_len)
        .expect("name bytes are ascii so index is a char boundary");
    let rest = inner
        .get(name_len..)
        .expect("name bytes are ascii so index is a char boundary");
    let val = lookup(name);

    if rest.is_empty() {
        return val.unwrap_or_default();
    }

    let unset_or_empty = val.as_deref().is_none_or(str::is_empty);
    if let Some(word) = rest.strip_prefix(":-") {
        return if unset_or_empty {
            expand_plain(word, lookup)
        } else {
            val.unwrap_or_default()
        };
    }
    if let Some(word) = rest.strip_prefix(":+") {
        return if unset_or_empty {
            String::new()
        } else {
            expand_plain(word, lookup)
        };
    }
    if let Some(word) = rest.strip_prefix('-') {
        return val.unwrap_or_else(|| expand_plain(word, lookup));
    }
    if let Some(word) = rest.strip_prefix('+') {
        return if val.is_some() {
            expand_plain(word, lookup)
        } else {
            String::new()
        };
    }

    format!("${{{inner}}}")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn lookup_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|&(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        move |name: &str| map.get(name).cloned()
    }

    fn exp(input: &str, pairs: &[(&str, &str)]) -> String {
        expand_plain(input, &lookup_from(pairs))
    }

    #[test]
    fn substitutes_a_bare_reference() {
        assert_eq!(exp("hi ${NAME}!", &[("NAME", "bob")]), "hi bob!");
    }

    #[test]
    fn unset_bare_reference_is_empty() {
        assert_eq!(exp("[${NOPE}]", &[]), "[]");
    }

    #[test]
    fn colon_dash_default_is_optional() {
        assert_eq!(exp("${VAR:-}", &[("VAR", "x")]), "x");
        assert_eq!(exp("${VAR:-}", &[]), "");
    }

    #[test]
    #[expect(
        clippy::literal_string_with_formatting_args,
        reason = "Shell parameter expansion is input to the parser"
    )]
    fn colon_dash_uses_default_when_unset_or_empty() {
        assert_eq!(exp("${V:-fallback}", &[]), "fallback");
        assert_eq!(exp("${V:-fallback}", &[("V", "")]), "fallback");
        assert_eq!(exp("${V:-fallback}", &[("V", "set")]), "set");
    }

    #[test]
    fn dash_uses_default_only_when_unset() {
        assert_eq!(exp("${V-fallback}", &[]), "fallback");
        assert_eq!(exp("${V-fallback}", &[("V", "")]), "");
        assert_eq!(exp("${V-fallback}", &[("V", "set")]), "set");
    }

    #[test]
    #[expect(
        clippy::literal_string_with_formatting_args,
        reason = "Shell parameter expansion is input to the parser"
    )]
    fn colon_plus_alternate_requires_non_empty() {
        assert_eq!(exp("${V:+yes}", &[("V", "x")]), "yes");
        assert_eq!(exp("${V:+yes}", &[("V", "")]), "");
        assert_eq!(exp("${V:+yes}", &[]), "");
    }

    #[test]
    fn plus_alternate_requires_only_set() {
        assert_eq!(exp("${V+yes}", &[("V", "x")]), "yes");
        assert_eq!(exp("${V+yes}", &[("V", "")]), "yes");
        assert_eq!(exp("${V+yes}", &[]), "");
    }

    #[test]
    fn default_word_is_expanded_recursively() {
        assert_eq!(exp("${A:-${B}}", &[("B", "deep")]), "deep");
        assert_eq!(exp("${A:-${B:-lit}}", &[]), "lit");
    }

    #[test]
    fn multiple_refs_and_surrounding_text() {
        assert_eq!(exp("${A}/${B}/end", &[("A", "x"), ("B", "y")]), "x/y/end");
    }

    #[test]
    fn expanded_value_is_not_word_split() {
        assert_eq!(exp("${V}", &[("V", "a b c")]), "a b c");
    }

    #[test]
    fn unterminated_brace_is_literal() {
        assert_eq!(exp("${A", &[("A", "x")]), "${A");
        assert_eq!(exp("pre ${A", &[("A", "x")]), "pre ${A");
    }

    #[test]
    fn unknown_operator_and_empty_name_are_left_verbatim() {
        assert_eq!(exp("${V:?nope}", &[("V", "x")]), "${V:?nope}");
        assert_eq!(exp("${V:=x}", &[("V", "y")]), "${V:=x}");
        assert_eq!(exp("${}", &[]), "${}");
    }

    #[test]
    fn bare_dollar_without_brace_is_left_alone() {
        assert_eq!(exp("$VAR and $$", &[("VAR", "x")]), "$VAR and $$");
    }

    #[test]
    fn backslash_dollar_is_a_literal_dollar() {
        assert_eq!(exp("\\${X}", &[("X", "v")]), "${X}");
        assert_eq!(exp("\\$X", &[("X", "v")]), "$X");
        assert_eq!(exp("\\${X}=${X}", &[("X", "v")]), "${X}=v");
        assert_eq!(exp("${A:-\\${B}}", &[("B", "v")]), "${B}");
    }

    #[test]
    fn lone_backslash_is_preserved() {
        assert_eq!(exp("a\\b", &[]), "a\\b");
    }
}
