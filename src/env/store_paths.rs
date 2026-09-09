use std::collections::{BTreeMap, BTreeSet};

const NIX_STORE_PREFIX: &str = "/nix/store/";
const NIX_STORE_HASH_LEN: usize = 32;

pub(super) fn from_env(vars: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    from_values(
        vars.values()
            .flat_map(|values| values.iter().map(String::as_str)),
    )
}

pub(super) fn from_values<'item>(values: impl Iterator<Item = &'item str>) -> Vec<String> {
    let mut paths = BTreeSet::new();
    for value in values {
        collect_from_str(value, &mut paths);
    }
    sorted(paths)
}

pub(super) fn merge_unique(
    current: Vec<String>,
    incoming: impl IntoIterator<Item = String>,
) -> Vec<String> {
    let mut paths: BTreeSet<String> = current.into_iter().collect();
    paths.extend(incoming);
    sorted(paths)
}

fn sorted(path_set: BTreeSet<String>) -> Vec<String> {
    path_set.into_iter().collect()
}

const fn is_store_name_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.' | b'_' | b'?' | b'=')
}

const fn is_store_hash_char(byte: u8) -> bool {
    matches!(byte, b'0'..=b'9' | b'a'..=b'd' | b'f'..=b'n' | b'p'..=b's' | b'v'..=b'z')
}

fn collect_from_str(text: &str, out: &mut BTreeSet<String>) {
    let mut offset = 0;
    while let Some(suffix) = text.get(offset..) {
        let Some(relative_start) = suffix.find(NIX_STORE_PREFIX) else {
            break;
        };
        let start = offset + relative_start;
        let hash_start = start + NIX_STORE_PREFIX.len();
        let hash_end = hash_start + NIX_STORE_HASH_LEN;
        let bytes = text.as_bytes();
        if bytes.len() <= hash_end
            || bytes.get(hash_end) != Some(&b'-')
            || !bytes[hash_start..hash_end]
                .iter()
                .all(|byte| is_store_hash_char(*byte))
        {
            offset = hash_start;
            continue;
        }

        let mut end = hash_end + 1;
        while end < bytes.len() && is_store_name_char(bytes[end]) {
            end += 1;
        }
        if end > hash_end + 1
            && let Some(matched) = text.get(start..end)
        {
            out.insert(matched.to_owned());
        }
        offset = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_nix_placeholder_hashes() {
        let paths = from_values(
            ["-fmacro-prefix-map=/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-gcc/include"]
                .into_iter(),
        );

        assert!(paths.is_empty());
    }

    #[test]
    fn keeps_valid_nix_store_hashes() {
        let path = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-demo";
        let paths = from_values([path].into_iter());

        assert_eq!(paths, [path]);
    }
}
