use super::set::ParsedEnv;
use anyhow::{Result, bail};
use std::collections::{HashMap, HashSet};

pub(super) fn parse_env_text(text: &str) -> Result<ParsedEnv> {
    let mut vars: HashMap<String, Vec<String>> = HashMap::new();
    let mut hard_replace = HashSet::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line
            .strip_prefix("export ")
            .map(str::trim_start)
            .unwrap_or(line);

        let Some((raw_key, raw_value)) = line.split_once('=') else {
            bail!("parsing variable from line: {line}")
        };
        let (key, replace) = match raw_key.strip_suffix(':') {
            Some(k) => (k.trim().to_owned(), true),
            None => (raw_key.trim().to_owned(), false),
        };
        if replace {
            hard_replace.insert(key.clone());
        }
        append_entry(&mut vars, key, split_env_value(&clean_env_value(raw_value)));
    }

    Ok(ParsedEnv::new(vars, hard_replace))
}

pub(super) fn split_env_value(value: &str) -> Vec<String> {
    value.split(':').map(str::to_owned).collect()
}

fn append_entry(vars: &mut HashMap<String, Vec<String>>, key: String, values: Vec<String>) {
    vars.entry(key)
        .and_modify(|current| current.extend(values.clone()))
        .or_insert(values);
}

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
