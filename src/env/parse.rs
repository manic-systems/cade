use std::collections::{
    BTreeMap,
    BTreeSet,
};

use anyhow::{
    Result,
    bail,
};

use super::set::ParsedEnv;

pub(super) fn parse_env_text(text: &str) -> Result<ParsedEnv> {
    let mut vars: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut hard_replace = BTreeSet::new();

    for raw_line in text.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let without_export = trimmed
            .strip_prefix("export ")
            .map_or(trimmed, str::trim_start);

        let Some((raw_key, raw_value)) = without_export.split_once('=') else {
            bail!("parsing variable from line: {without_export}")
        };
        let (key, replace) = raw_key.strip_suffix(':').map_or_else(
            || (raw_key.trim().to_owned(), false),
            |stripped| (stripped.trim().to_owned(), true),
        );
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

fn append_entry(vars: &mut BTreeMap<String, Vec<String>>, key: String, values: Vec<String>) {
    vars.entry(key)
        .and_modify(|current| current.extend(values.clone()))
        .or_insert(values);
}

fn clean_env_value(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(inner) = trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        return inner.to_owned();
    }
    if let Some(inner) = trimmed
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        return inner.to_owned();
    }
    match trimmed.split_once(" #") {
        Some((before, _)) => before.trim_end().to_owned(),
        None => trimmed.to_owned(),
    }
}
