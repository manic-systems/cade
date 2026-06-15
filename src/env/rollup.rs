use crate::{
    env::is_shell_managed,
    types::{CadeLayer, InnerHook},
};
use std::collections::{HashMap, HashSet};

pub struct RollupResult {
    env: HashMap<String, Vec<String>>,
    absorb: HashSet<String>,
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
    pub fn env(&self) -> &HashMap<String, Vec<String>> {
        &self.env
    }

    pub fn absorb(&self) -> &HashSet<String> {
        &self.absorb
    }

    pub fn unset(&self) -> &[String] {
        &self.unset
    }

    pub fn hooks(&self) -> &[InnerHook] {
        &self.hooks
    }

    pub fn purified(&self) -> bool {
        self.purified
    }

    pub fn set_keys(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self.env.keys().map(String::as_str).collect();
        keys.sort_unstable();
        keys
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
    let mut env: HashMap<String, Vec<String>> = HashMap::new();
    let mut cleared: HashSet<String> = HashSet::new();
    let mut absorb: HashSet<String> = HashSet::new();
    let mut hooks = Vec::new();
    let mut concat_active: HashSet<String> = PATH_LIKE.iter().map(|s| s.to_string()).collect();

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

        for (k, v, replaces) in layer.envs.into_parsed_env().into_entries() {
            if is_shell_managed(&k) {
                continue;
            }
            cleared.remove(&k);
            let is_concat = !replaces && concat_active.contains(&k);
            if is_concat {
                absorb.insert(k.clone());
                let entry = env.entry(k).or_default();
                let mut combined = v;
                combined.append(entry);
                *entry = combined;
            } else if !replaces && SPACE_JOINED.contains(&k.as_str()) {
                absorb.remove(&k);
                let value = join_space_values(v);
                if let Some(previous) = env.get(&k).map(|values| join_space_values(values.clone()))
                {
                    env.insert(k, vec![join_space_values(vec![value, previous])]);
                } else {
                    env.insert(k, vec![value]);
                }
            } else {
                absorb.remove(&k);
                env.insert(k, v);
            }
        }

        if !purified && layer.purify {
            purified = true;
        }
        hooks.extend(layer.hooks);
    }

    let mut unset: Vec<String> = cleared
        .into_iter()
        .filter(|k| !env.contains_key(k))
        .collect();
    unset.sort_unstable();

    RollupResult {
        env,
        absorb,
        unset,
        hooks,
        purified,
    }
}

fn join_space_values(values: Vec<String>) -> String {
    values
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{env::EnvSet, types::CadeAction};
    use std::path::Path;

    fn env_layer(pairs: &[(&str, &str)]) -> CadeLayer {
        let mut layer = CadeLayer::new(0, Path::new("/"));
        layer.push_action(CadeAction::Environ(env_set(pairs)));
        layer
    }

    fn env_set(pairs: &[(&str, &str)]) -> EnvSet {
        let mut text = String::new();
        for (key, value) in pairs {
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
        let r = rollup_envs(vec![parent, child]);
        assert_eq!(
            r.values("PATH"),
            Some(&["/child/bin".into(), "/parent/bin".into()][..])
        );
        assert!(r.absorbs("PATH"), "PATH should absorb ambient");
        assert_eq!(r.values("ONLY_PARENT"), Some(&["p".into()][..]));
        assert_eq!(r.values("ONLY_CHILD"), Some(&["c".into()][..]));
        assert!(!r.absorbs("ONLY_PARENT"));
        assert!(!r.purified());
    }

    #[test]
    fn scalar_var_replaces_child_wins() {
        let parent = env_layer(&[("EDITOR", "nano")]);
        let child = env_layer(&[("EDITOR", "vim")]);
        let r = rollup_envs(vec![parent, child]);
        assert_eq!(r.values("EDITOR"), Some(&["vim".into()][..]));
        assert!(!r.absorbs("EDITOR"));
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
        let r = rollup_envs(vec![parent, child]);

        assert_eq!(
            r.values("NIX_LDFLAGS").unwrap(),
            vec!["-L/child/lib -L/parent/lib -rpath /parent/lib"]
        );
        assert_eq!(
            r.values("NIX_CFLAGS_COMPILE").unwrap(),
            vec!["-isystem /child/include -isystem /parent/include"]
        );
        assert_eq!(
            r.values("NIX_HARDENING_ENABLE").unwrap(),
            vec!["relro fortify stackprotector"]
        );
        assert!(!r.absorbs("NIX_LDFLAGS"));
    }

    #[test]
    fn nix_wrapper_scalar_vars_replace_child_wins() {
        let parent = env_layer(&[("NIX_CC", "/parent/cc"), ("NIX_STORE", "/parent/store")]);
        let child = env_layer(&[("NIX_CC", "/child/cc"), ("NIX_STORE", "/child/store")]);
        let r = rollup_envs(vec![parent, child]);

        assert_eq!(r.values("NIX_CC"), Some(&["/child/cc".into()][..]));
        assert_eq!(r.values("NIX_STORE"), Some(&["/child/store".into()][..]));
    }

    #[test]
    fn hard_replace_overrides_concat_default() {
        let parent = env_layer(&[("PATH", "/parent/bin")]);
        let mut child = CadeLayer::new(1, Path::new("/"));
        child.push_action(CadeAction::Environ(
            EnvSet::from_envs("PATH:=/only/child\n").unwrap(),
        ));
        let r = rollup_envs(vec![parent, child]);
        assert_eq!(r.values("PATH"), Some(&["/only/child".into()][..]));
        assert!(!r.absorbs("PATH"), "hard replace drops ambient");
    }

    #[test]
    fn concat_directive_marks_custom_var() {
        let mut parent = env_layer(&[("MYLIST", "/p")]);
        parent.push_action(CadeAction::Concat(vec!["MYLIST".to_string()]));
        let child = env_layer(&[("MYLIST", "/c")]);
        let r = rollup_envs(vec![parent, child]);
        assert_eq!(r.values("MYLIST"), Some(&["/c".into(), "/p".into()][..]));
        assert!(r.absorbs("MYLIST"));
    }

    #[test]
    fn clear_removes_inherited_and_is_reported_as_unset() {
        let parent = env_layer(&[("DROP_ME", "x"), ("KEEP", "y")]);
        let mut child = CadeLayer::new(1, Path::new("/"));
        child.push_action(CadeAction::Clear(vec!["DROP_ME".into()]));
        let r = rollup_envs(vec![parent, child]);
        assert!(!r.contains_key("DROP_ME"));
        assert!(r.contains_key("KEEP"));
        assert_eq!(r.unset(), ["DROP_ME"]);
    }

    #[test]
    fn clear_then_reset_in_later_layer_cancels_unset() {
        let l1 = env_layer(&[("X", "1")]);
        let mut l2 = CadeLayer::new(1, Path::new("/"));
        l2.push_action(CadeAction::Clear(vec!["X".into()]));
        let l3 = env_layer(&[("X", "2")]);
        let r = rollup_envs(vec![l1, l2, l3]);
        assert_eq!(r.values("X"), Some(&["2".into()][..]));
        assert!(
            r.unset().is_empty(),
            "X was re-set, so it must not be unset"
        );
    }

    #[test]
    fn pure_flag_does_not_drop_inherited_layers() {
        let parent = env_layer(&[("FROM_PARENT", "kept")]);
        let mut child = CadeLayer::new(1, Path::new("/"));
        child.push_action(CadeAction::Purify);
        child.push_action(CadeAction::Environ(env_set(&[("FROM_CHILD", "c")])));
        let r = rollup_envs(vec![parent, child]);
        assert!(r.purified());
        assert_eq!(r.values("FROM_PARENT"), Some(&["kept".into()][..]));
        assert_eq!(r.values("FROM_CHILD"), Some(&["c".into()][..]));
    }
}
