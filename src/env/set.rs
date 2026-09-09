use std::{
    collections::{
        BTreeMap,
        BTreeSet,
    },
    mem::take,
};

use serde::{
    Deserialize,
    Serialize,
};

use super::{
    parse,
    store_paths,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnvSet {
    vars:            BTreeMap<String, Vec<String>>,
    #[serde(default, rename = "hard")]
    hard_replace:    BTreeSet<String>,
    #[serde(default)]
    clears:          BTreeSet<String>,
    #[serde(default)]
    nix_store_paths: Vec<String>,
}

pub(super) struct ParsedEnv {
    vars:         BTreeMap<String, Vec<String>>,
    hard_replace: BTreeSet<String>,
    clears:       BTreeSet<String>,
}

impl ParsedEnv {
    pub(super) const fn new(
        vars: BTreeMap<String, Vec<String>>,
        hard_replace: BTreeSet<String>,
    ) -> Self {
        Self {
            vars,
            hard_replace,
            clears: BTreeSet::new(),
        }
    }

    pub(super) fn into_entries(self) -> impl Iterator<Item = (String, Vec<String>, bool)> {
        let hard_replace = self.hard_replace;
        self.vars.into_iter().map(move |(key, values)| {
            let replaces = hard_replace.contains(&key);
            (key, values, replaces)
        })
    }

    pub(super) fn clears(&self) -> impl Iterator<Item = &str> {
        self.clears.iter().map(String::as_str)
    }
}

pub struct EnvSetMerge {
    pub store_paths: Vec<String>,
    pub clears:      Vec<String>,
    pub sets:        Vec<String>,
}

impl EnvSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_envs(text: &str) -> anyhow::Result<Self> {
        let parts = parse::parse_env_text(text)?;
        let mut env = Self {
            vars:            parts.vars,
            hard_replace:    parts.hard_replace,
            clears:          BTreeSet::new(),
            nix_store_paths: Vec::new(),
        };
        env.refresh_store_paths();
        Ok(env)
    }

    pub fn from_captured_parts(
        vars: BTreeMap<String, Vec<String>>,
        clears: BTreeSet<String>,
    ) -> Self {
        let mut env = Self::from_plain_vars(vars);
        env.clears = clears
            .into_iter()
            .filter(|key| !env.vars.contains_key(key))
            .collect();
        env.refresh_store_paths();
        env
    }

    pub fn merge_loaded(&mut self, other: Self) {
        let Self {
            vars,
            hard_replace,
            clears,
            nix_store_paths,
        } = other;

        for key in clears {
            self.vars.remove(&key);
            self.hard_replace.remove(&key);
            self.clears.insert(key);
        }
        for (key, values) in vars {
            self.clears.remove(&key);
            append_entry(&mut self.vars, key, values);
        }
        self.hard_replace.extend(hard_replace);
        self.merge_store_paths(nix_store_paths);
    }

    pub fn merge_layer_env(&mut self, other: Self) -> EnvSetMerge {
        let Self {
            vars,
            hard_replace,
            clears,
            nix_store_paths,
        } = other;

        let mut merged_clears = Vec::new();
        let mut merged_sets = Vec::new();
        self.hard_replace.extend(hard_replace);
        for key in clears {
            self.vars.remove(&key);
            self.hard_replace.remove(&key);
            self.clears.insert(key.clone());
            merged_clears.push(key);
        }
        for (key, values) in vars {
            self.clears.remove(&key);
            merged_sets.push(key.clone());
            self.append_values(key, values);
        }
        EnvSetMerge {
            store_paths: nix_store_paths,
            clears:      merged_clears,
            sets:        merged_sets,
        }
    }

    pub fn add_literal_export(&mut self, key: String, value: &str) {
        self.append_values(key, parse::split_env_value(value));
    }

    pub fn prepend_path_entries(&mut self, values: Vec<String>) {
        self.prepend_values("PATH".to_owned(), values);
    }

    pub fn expand_values<Expander: FnMut(&str) -> String>(&mut self, mut expand: Expander) {
        for values in self.vars.values_mut() {
            let value = expand(&values.join(":"));
            *values = parse::split_env_value(&value);
        }
        self.refresh_store_paths();
    }

    pub fn derived_store_paths(&self) -> Vec<String> {
        store_paths::from_env(&self.vars)
    }

    pub fn retain_store_path(&mut self, path: String) {
        self.merge_store_paths(vec![path]);
    }

    pub(super) fn into_parsed_env(self) -> ParsedEnv {
        ParsedEnv {
            vars:         self.vars,
            hard_replace: self.hard_replace,
            clears:       self.clears,
        }
    }

    const fn from_plain_vars(vars: BTreeMap<String, Vec<String>>) -> Self {
        Self {
            vars,
            hard_replace: BTreeSet::new(),
            clears: BTreeSet::new(),
            nix_store_paths: Vec::new(),
        }
    }

    fn append_values(&mut self, key: String, values: Vec<String>) {
        self.clears.remove(&key);
        self.record_store_paths(&values);
        append_entry(&mut self.vars, key, values);
    }

    fn prepend_values(&mut self, key: String, mut values: Vec<String>) {
        self.clears.remove(&key);
        self.record_store_paths(&values);
        let entry = self.vars.entry(key).or_default();
        values.append(entry);
        *entry = values;
    }

    fn record_store_paths(&mut self, values: &[String]) {
        self.merge_store_paths(store_paths::from_values(values.iter().map(String::as_str)));
    }

    fn merge_store_paths(&mut self, incoming: Vec<String>) {
        self.nix_store_paths = store_paths::merge_unique(take(&mut self.nix_store_paths), incoming);
    }

    fn refresh_store_paths(&mut self) {
        self.nix_store_paths = self.derived_store_paths();
    }
}

fn append_entry(vars: &mut BTreeMap<String, Vec<String>>, key: String, values: Vec<String>) {
    vars.entry(key)
        .and_modify(|current| current.extend(values.clone()))
        .or_insert(values);
}

#[cfg(test)]
mod tests {
    use super::*;

    const STORE_PATH: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-demo";

    fn values<'env>(env: &'env EnvSet, key: &str) -> Option<&'env [String]> {
        env.vars.get(key).map(Vec::as_slice)
    }

    #[test]
    fn parses_kv_skips_comments_and_blanks() {
        let env = EnvSet::from_envs("# comment\n\nFOO=bar\n  BAZ=qux  \n").unwrap();
        assert_eq!(values(&env, "FOO").unwrap(), ["bar"]);
        assert_eq!(values(&env, "BAZ").unwrap(), ["qux"]);
        assert_eq!(env.vars.len(), 2);
        assert!(env.hard_replace.is_empty());
    }

    #[test]
    fn splits_colon_lists_and_merges_duplicate_keys() {
        let env = EnvSet::from_envs("PATH=/a:/b\nPATH=/c").unwrap();
        assert_eq!(values(&env, "PATH").unwrap(), ["/a", "/b", "/c"]);
    }

    #[test]
    fn hard_replace_notation_is_recorded() {
        let env = EnvSet::from_envs("PATH:=/only/this\nFOO=bar").unwrap();
        assert_eq!(values(&env, "PATH").unwrap(), ["/only/this"]);
        assert!(env.hard_replace.contains("PATH"), "PATH:= should replace");
        assert!(!env.hard_replace.contains("FOO"), "FOO= should compose");
    }

    #[test]
    fn handles_export_prefix_quotes_and_inline_comments() {
        let env = EnvSet::from_envs(
            "export FOO=bar\nQUOTED=\"hello world\"\nSQ='a b'\nWITH=val # trailing note\n",
        )
        .unwrap();
        assert_eq!(values(&env, "FOO").unwrap(), ["bar"]);
        assert_eq!(values(&env, "QUOTED").unwrap(), ["hello world"]);
        assert_eq!(values(&env, "SQ").unwrap(), ["a b"]);
        assert_eq!(values(&env, "WITH").unwrap(), ["val"]);
    }

    #[test]
    fn hash_inside_quotes_is_kept() {
        let env = EnvSet::from_envs("TOKEN=\"a#b\"\nFRAG=x#y\n").unwrap();
        assert_eq!(values(&env, "TOKEN").unwrap(), ["a#b"]);
        assert_eq!(values(&env, "FRAG").unwrap(), ["x#y"]);
    }

    #[test]
    fn errors_on_line_without_equals() {
        EnvSet::from_envs("NOT_A_PAIR").unwrap_err();
    }

    #[test]
    fn preserves_hostile_value_verbatim() {
        let env = EnvSet::from_envs("EVIL=$(touch /tmp/pwned)").unwrap();
        assert_eq!(values(&env, "EVIL").unwrap(), ["$(touch /tmp/pwned)"]);
    }

    #[test]
    fn appended_values_update_store_paths() {
        let mut env = EnvSet::new();
        env.add_literal_export("TOOL".to_owned(), STORE_PATH);
        assert_eq!(env.nix_store_paths, [STORE_PATH]);
    }

    #[test]
    fn merge_loaded_preserves_cleared_store_paths() {
        let mut out = EnvSet::new();
        let other = EnvSet::from_plain_vars(BTreeMap::from([("TOOL".to_owned(), vec![
            STORE_PATH.to_owned(),
        ])]));
        out.merge_loaded(other);
        assert!(out.nix_store_paths.is_empty());
    }
}
