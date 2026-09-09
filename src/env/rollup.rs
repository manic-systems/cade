use crate::{
    env::delta::is_shell_managed,
    types::{hook::InnerHook, layer::CadeLayer},
};
use std::collections::{BTreeMap, BTreeSet};

pub struct RollupResult {
    env: BTreeMap<String, Vec<String>>,
    absorb: BTreeSet<String>,
    unset: Vec<String>,
    hooks: Vec<InnerHook>,
    purified: bool,
}

const PATH_LIKE: &[&str] = &[
    "PATH",
    "MANPATH",
    "INFOPATH",
    "CDPATH",
    "LD_LIBRARY_PATH",
    "DYLD_LIBRARY_PATH",
    "DYLD_FALLBACK_LIBRARY_PATH",
    "LIBRARY_PATH",
    "CPATH",
    "C_INCLUDE_PATH",
    "CPLUS_INCLUDE_PATH",
    "OBJC_INCLUDE_PATH",
    "PKG_CONFIG_PATH",
    "CMAKE_PREFIX_PATH",
    "ACLOCAL_PATH",
    "XDG_DATA_DIRS",
    "XDG_CONFIG_DIRS",
    "TERMINFO_DIRS",
];

const SPACE_JOINED: &[&str] = &["NIX_CFLAGS_COMPILE", "NIX_HARDENING_ENABLE", "NIX_LDFLAGS"];

impl RollupResult {
    pub const fn env(&self) -> &BTreeMap<String, Vec<String>> {
        &self.env
    }

    pub const fn absorb(&self) -> &BTreeSet<String> {
        &self.absorb
    }

    pub fn unset(&self) -> &[String] {
        &self.unset
    }

    pub fn hooks(&self) -> &[InnerHook] {
        &self.hooks
    }

    pub const fn purified(&self) -> bool {
        self.purified
    }

    pub fn set_keys(&self) -> Vec<&str> {
        self.env.keys().map(String::as_str).collect()
    }

    #[cfg(test)]
    fn values(&self, key: &str) -> Option<&[String]> {
        self.env.get(key).map(Vec::as_slice)
    }

    #[cfg(test)]
    fn contains_key(&self, key: &str) -> bool {
        self.env.contains_key(key)
    }

    #[cfg(test)]
    fn absorbs(&self, key: &str) -> bool {
        self.absorb.contains(key)
    }
}

pub fn rollup_envs(cade_layers: Vec<CadeLayer>) -> RollupResult {
    let mut purified = false;
    let mut env: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut cleared: BTreeSet<String> = BTreeSet::new();
    let mut absorb: BTreeSet<String> = BTreeSet::new();
    let mut hooks = Vec::new();
    let mut concat_active: BTreeSet<String> = PATH_LIKE.iter().map(ToString::to_string).collect();

    for layer in cade_layers {
        concat_active.extend(layer.concat);

        for var in &layer.clears {
            if is_shell_managed(var) {
                continue;
            }
            env.remove(var);
            absorb.remove(var);
            cleared.insert(var.clone());
        }

        let parsed = layer.envs.into_parsed_env();
        for var in parsed.clears() {
            if is_shell_managed(var) {
                continue;
            }
            env.remove(var);
            absorb.remove(var);
            cleared.insert(var.to_owned());
        }

        for (key, incoming, replaces) in parsed.into_entries() {
            if is_shell_managed(&key) {
                continue;
            }
            cleared.remove(&key);
            let is_concat = !replaces && concat_active.contains(&key);
            if is_concat {
                absorb.insert(key.clone());
                let entry = env.entry(key).or_default();
                let mut combined = incoming;
                combined.append(entry);
                *entry = combined;
            } else if !replaces && SPACE_JOINED.contains(&key.as_str()) {
                absorb.remove(&key);
                let value = join_space_values(&incoming);
                if let Some(previous) = env.get(&key).map(|existing| join_space_values(existing)) {
                    env.insert(key, vec![join_space_values(&[value, previous])]);
                } else {
                    env.insert(key, vec![value]);
                }
            } else {
                absorb.remove(&key);
                env.insert(key, incoming);
            }
        }

        purified |= layer.purify;
        hooks.extend(layer.hooks);
    }

    let unset: Vec<String> = cleared
        .into_iter()
        .filter(|name| !env.contains_key(name))
        .collect();

    RollupResult {
        env,
        absorb,
        unset,
        hooks,
        purified,
    }
}

fn join_space_values(values: &[String]) -> String {
    values
        .iter()
        .filter(|value| !value.is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{env::set::EnvSet, types::layer::CadeAction};

    fn env_layer(pairs: &[(&str, &str)]) -> CadeLayer {
        let mut layer = CadeLayer::default();
        layer
            .push_action(&CadeAction::Environ(env_set(pairs)))
            .unwrap();
        layer
    }

    fn env_set(pairs: &[(&str, &str)]) -> EnvSet {
        let mut text = String::new();
        for &(key, value) in pairs {
            text.push_str(key);
            text.push('=');
            text.push_str(value);
            text.push('\n');
        }
        EnvSet::from_envs(&text).unwrap()
    }

    #[test]
    fn path_like_vars_concat_child_first() {
        let parent = env_layer(&[("PATH", "/parent/bin"), ("ONLY_PARENT", "p")]);
        let child = env_layer(&[("PATH", "/child/bin"), ("ONLY_CHILD", "c")]);
        let rollup = rollup_envs(vec![parent, child]);
        assert_eq!(
            rollup.values("PATH"),
            Some(&["/child/bin".into(), "/parent/bin".into()][..])
        );
        assert!(rollup.absorbs("PATH"), "PATH should absorb ambient");
        assert_eq!(rollup.values("ONLY_PARENT"), Some(&["p".into()][..]));
        assert_eq!(rollup.values("ONLY_CHILD"), Some(&["c".into()][..]));
        assert!(!rollup.absorbs("ONLY_PARENT"));
        assert!(!rollup.purified());
    }

    #[test]
    fn scalar_var_replaces_child_wins() {
        let parent = env_layer(&[("EDITOR", "nano")]);
        let child = env_layer(&[("EDITOR", "vim")]);
        let rollup = rollup_envs(vec![parent, child]);
        assert_eq!(rollup.values("EDITOR"), Some(&["vim".into()][..]));
        assert!(!rollup.absorbs("EDITOR"));
    }

    #[test]
    fn nix_wrapper_flags_stack_child_first() {
        let parent = env_layer(&[
            ("NIX_LDFLAGS", "-L/parent/lib -rpath /parent/lib"),
            ("NIX_CFLAGS_COMPILE", "-isystem /parent/include"),
            ("NIX_HARDENING_ENABLE", "fortify stackprotector"),
        ]);
        let child = env_layer(&[
            ("NIX_LDFLAGS", "-L/child/lib"),
            ("NIX_CFLAGS_COMPILE", "-isystem /child/include"),
            ("NIX_HARDENING_ENABLE", "relro"),
        ]);
        let rollup = rollup_envs(vec![parent, child]);

        assert_eq!(
            rollup.values("NIX_LDFLAGS").unwrap(),
            vec!["-L/child/lib -L/parent/lib -rpath /parent/lib"]
        );
        assert_eq!(
            rollup.values("NIX_CFLAGS_COMPILE").unwrap(),
            vec!["-isystem /child/include -isystem /parent/include"]
        );
        assert_eq!(
            rollup.values("NIX_HARDENING_ENABLE").unwrap(),
            vec!["relro fortify stackprotector"]
        );
        assert!(!rollup.absorbs("NIX_LDFLAGS"));
    }

    #[test]
    fn nix_wrapper_scalar_vars_replace_child_wins() {
        let parent = env_layer(&[("NIX_CC", "/parent/cc"), ("NIX_STORE", "/parent/store")]);
        let child = env_layer(&[("NIX_CC", "/child/cc"), ("NIX_STORE", "/child/store")]);
        let rollup = rollup_envs(vec![parent, child]);

        assert_eq!(rollup.values("NIX_CC"), Some(&["/child/cc".into()][..]));
        assert_eq!(
            rollup.values("NIX_STORE"),
            Some(&["/child/store".into()][..])
        );
    }

    #[test]
    fn hard_replace_overrides_concat_default() {
        let parent = env_layer(&[("PATH", "/parent/bin")]);
        let mut child = CadeLayer::default();
        child
            .push_action(&CadeAction::Environ(
                EnvSet::from_envs("PATH:=/only/child\n").unwrap(),
            ))
            .unwrap();
        let rollup = rollup_envs(vec![parent, child]);
        assert_eq!(rollup.values("PATH"), Some(&["/only/child".into()][..]));
        assert!(!rollup.absorbs("PATH"), "hard replace drops ambient");
    }

    #[test]
    fn concat_directive_marks_custom_var() {
        let mut parent = env_layer(&[("MYLIST", "/p")]);
        parent
            .push_action(&CadeAction::Concat(vec!["MYLIST".to_owned()]))
            .unwrap();
        let child = env_layer(&[("MYLIST", "/c")]);
        let rollup = rollup_envs(vec![parent, child]);
        assert_eq!(
            rollup.values("MYLIST"),
            Some(&["/c".into(), "/p".into()][..])
        );
        assert!(rollup.absorbs("MYLIST"));
    }

    #[test]
    fn clear_removes_inherited_and_is_reported_as_unset() {
        let parent = env_layer(&[("DROP_ME", "x"), ("KEEP", "y")]);
        let mut child = CadeLayer::default();
        child
            .push_action(&CadeAction::Clear(vec!["DROP_ME".into()]))
            .unwrap();
        let rollup = rollup_envs(vec![parent, child]);
        assert!(!rollup.contains_key("DROP_ME"));
        assert!(rollup.contains_key("KEEP"));
        assert_eq!(rollup.unset(), ["DROP_ME"]);
    }

    #[test]
    fn clear_then_reset_in_later_layer_cancels_unset() {
        let first = env_layer(&[("X", "1")]);
        let mut second = CadeLayer::default();
        second
            .push_action(&CadeAction::Clear(vec!["X".into()]))
            .unwrap();
        let third = env_layer(&[("X", "2")]);
        let rollup = rollup_envs(vec![first, second, third]);
        assert_eq!(rollup.values("X"), Some(&["2".into()][..]));
        assert!(
            rollup.unset().is_empty(),
            "X was re-set, so it must not be unset"
        );
    }

    #[test]
    fn pure_flag_does_not_drop_inherited_layers() {
        let parent = env_layer(&[("FROM_PARENT", "kept")]);
        let mut child = CadeLayer::default();
        child.push_action(&CadeAction::Purify).unwrap();
        child
            .push_action(&CadeAction::Environ(env_set(&[("FROM_CHILD", "c")])))
            .unwrap();
        let rollup = rollup_envs(vec![parent, child]);
        assert!(rollup.purified());
        assert_eq!(rollup.values("FROM_PARENT"), Some(&["kept".into()][..]));
        assert_eq!(rollup.values("FROM_CHILD"), Some(&["c".into()][..]));
    }
}
