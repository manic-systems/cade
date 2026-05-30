use std::collections::HashMap;

use crate::types::EnvSet;
use anyhow::{Result, bail};

const NIX_STORE_PREFIX: &str = "/nix/store/";
const NIX_STORE_HASH_LEN: usize = 32;

/// Normalize a raw .env value
fn clean_env_value(raw: &str) -> String {
    let v = raw.trim();
    let bytes = v.as_bytes();
    if v.len() >= 2 {
        let (first, last) = (bytes[0], bytes[v.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return v[1..v.len() - 1].to_string();
        }
    }
    match v.split_once(" #") {
        Some((before, _)) => before.trim_end().to_string(),
        None => v.to_string(),
    }
}

impl EnvSet {
    pub fn from_envs(text: &str) -> Result<EnvSet> {
        let mut vars: HashMap<String, Vec<String>> = HashMap::new();
        let mut hard = std::collections::HashSet::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // tolerate a leading `export `
            let line = line
                .strip_prefix("export ")
                .map(str::trim_start)
                .unwrap_or(line);

            let Some((raw_key, raw_value)) = line.split_once('=') else {
                bail!("parsing variable from line: {line}")
            };
            // a `:` suffix (`KEY:=value`) marks a hard replace
            let (key, is_hard) = match raw_key.strip_suffix(':') {
                Some(k) => (k.trim().to_owned(), true),
                None => (raw_key.trim().to_owned(), false),
            };
            let value = clean_env_value(raw_value);
            let values: Vec<String> = value.split(':').map(|s| s.to_owned()).collect();
            if is_hard {
                hard.insert(key.clone());
            }
            vars.entry(key)
                .and_modify(|v: &mut Vec<String>| v.extend(values.clone()))
                .or_insert(values);
        }
        let mut env = EnvSet {
            vars,
            hard,
            nix_store_paths: Vec::new(),
        };
        env.nix_store_paths = nix_store_paths_from_env_values(&env);
        Ok(env)
    }
}

fn is_store_name_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.' | b'_' | b'?' | b'=')
}

fn collect_store_paths_from_str(text: &str, out: &mut std::collections::HashSet<String>) {
    let mut offset = 0;
    while let Some(relative_start) = text[offset..].find(NIX_STORE_PREFIX) {
        let start = offset + relative_start;
        let hash_start = start + NIX_STORE_PREFIX.len();
        let hash_end = hash_start + NIX_STORE_HASH_LEN;
        let bytes = text.as_bytes();
        if bytes.len() <= hash_end || bytes.get(hash_end) != Some(&b'-') {
            offset = hash_start;
            continue;
        }

        let mut end = hash_end + 1;
        while end < bytes.len() && is_store_name_char(bytes[end]) {
            end += 1;
        }
        if end > hash_end + 1 {
            out.insert(text[start..end].to_string());
        }
        offset = end;
    }
}

pub fn nix_store_paths_from_env_values(env: &EnvSet) -> Vec<String> {
    let mut paths = std::collections::HashSet::new();
    for values in env.vars.values() {
        for value in values {
            collect_store_paths_from_str(value, &mut paths);
        }
    }
    let mut paths: Vec<String> = paths.into_iter().collect();
    paths.sort_unstable();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kv_skips_comments_and_blanks() {
        let env = EnvSet::from_envs("# comment\n\nFOO=bar\n  BAZ=qux  \n").unwrap();
        assert_eq!(env.vars["FOO"], vec!["bar"]);
        assert_eq!(env.vars["BAZ"], vec!["qux"]);
        assert_eq!(env.vars.len(), 2);
        assert!(env.hard.is_empty());
    }

    #[test]
    fn splits_colon_lists_and_merges_duplicate_keys() {
        let env = EnvSet::from_envs("PATH=/a:/b\nPATH=/c").unwrap();
        assert_eq!(env.vars["PATH"], vec!["/a", "/b", "/c"]);
    }

    #[test]
    fn hard_replace_notation_is_recorded() {
        let env = EnvSet::from_envs("PATH:=/only/this\nFOO=bar").unwrap();
        assert_eq!(env.vars["PATH"], vec!["/only/this"]);
        assert!(env.hard.contains("PATH"), "PATH:= should be hard");
        assert!(!env.hard.contains("FOO"), "FOO= should not be hard");
    }

    #[test]
    fn handles_export_prefix_quotes_and_inline_comments() {
        let env = EnvSet::from_envs(
            "export FOO=bar\nQUOTED=\"hello world\"\nSQ='a b'\nWITH=val # trailing note\n",
        )
        .unwrap();
        assert_eq!(env.vars["FOO"], vec!["bar"]);
        assert_eq!(env.vars["QUOTED"], vec!["hello world"]);
        assert_eq!(env.vars["SQ"], vec!["a b"]);
        assert_eq!(env.vars["WITH"], vec!["val"]);
    }

    #[test]
    fn hash_inside_quotes_is_kept() {
        let env = EnvSet::from_envs("TOKEN=\"a#b\"\nFRAG=x#y\n").unwrap();
        assert_eq!(env.vars["TOKEN"], vec!["a#b"]);
        // a `#` without a preceding space is part of the value, not a comment
        assert_eq!(env.vars["FRAG"], vec!["x#y"]);
    }

    #[test]
    fn errors_on_line_without_equals() {
        assert!(EnvSet::from_envs("NOT_A_PAIR").is_err());
    }

    #[test]
    fn preserves_hostile_value_verbatim() {
        // parsing must not interpret shell metacharacters; quoting is the emitter's job
        let env = EnvSet::from_envs("EVIL=$(touch /tmp/pwned)").unwrap();
        assert_eq!(env.vars["EVIL"], vec!["$(touch /tmp/pwned)"]);
    }
}
