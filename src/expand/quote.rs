use super::{Lookup, eval::expand_with};

pub(super) fn expand_shell_args(input: &str, lookup: Lookup<'_>) -> String {
    expand_with(input, lookup, &|v| {
        shlex::try_quote(&v).map(|c| c.into_owned()).unwrap_or(v)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    fn args(input: &str, pairs: &[(&str, &str)]) -> Vec<String> {
        let expanded = expand_shell_args(input, &lookup_from(pairs));
        shlex::split(&expanded).expect("balanced quotes")
    }

    #[test]
    fn call_arg_single_backslash_escapes_uniformly() {
        assert_eq!(
            args("echo \\${VAR}", &[("VAR", "v")]),
            vec!["echo", "${VAR}"]
        );
        assert_eq!(args("echo ${VAR}", &[("VAR", "v")]), vec!["echo", "v"]);
    }

    #[test]
    fn call_arg_value_is_one_token_even_with_spaces_or_quotes() {
        assert_eq!(args("run ${A}", &[("A", "a b c")]), vec!["run", "a b c"]);
        assert_eq!(args("run ${A}", &[("A", "a'b")]), vec!["run", "a'b"]);
        assert_eq!(
            args("run \"x y\" ${A}", &[("A", "z")]),
            vec!["run", "x y", "z"]
        );
    }
}
