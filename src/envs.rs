use std::collections::HashMap;

use crate::types::EnvSet;
use anyhow::{Context, Result, anyhow, bail};

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
        Ok(EnvSet { vars, hard })
    }
    pub fn from_json(raw: &[u8]) -> Result<EnvSet> {
        let json: serde_json::Value = serde_json::from_slice(raw).context("parsing json")?;
        if json.is_object()
            && let Some(all_vars) = json.get("variables")
        {
            let vars = all_vars
                .as_object()
                .map(|inner| {
                    inner
                        .iter()
                        .filter(|(var, _)| {
                            !(var.starts_with("NIX_")
                                || var.starts_with("output")
                                || var.starts_with("deps")
                                || var.starts_with("enable")
                                || var.ends_with("Inputs")
                                || var.ends_with("Flags")
                                || var.ends_with("TYPE")
                                || var.to_lowercase().contains("phase")
                                || matches!(
                                    var.as_str(),
                                    "SHELL"
                                        | "pkg"
                                        | "prefix"
                                        | "guess"
                                        | "_substituteStream_has_warned_replace_deprecation"
                                        | "LINENO"
                                        | "OPTERROR"
                                        | "OLDPWD"
                                        | "BASH"
                                        | "IFS"
                                        | "PS4"
                                        | "initialPath"
                                        | "out"
                                        | "shell"
                                        | "STRINGS"
                                        | "stdenv"
                                        | "builder"
                                        | "PWD"
                                        | "SOURCE_DATE_EPOCH"
                                        | "CXX"
                                        | "TEMPDIR"
                                        | "system"
                                        | "HOST_PATH"
                                        | "doInstallCheck"
                                        | "buildCommandPath"
                                        | "LS_COLORS"
                                        | "cmakeFlakes"
                                        | "TMPDIR"
                                        | "LD"
                                        | "READELF"
                                        | "doCheck"
                                        | "SIZE"
                                        | "propagatedNativeBuildInputs"
                                        | "strictDeps"
                                        | "AR"
                                        | "AS"
                                        | "TEMP"
                                        | "SHLVL"
                                        | "NM"
                                        | "patches"
                                        | "passAsFile"
                                        | "buildInputs"
                                        | "SSL_CERT_FILE"
                                        | "OBJCOPY"
                                        | "STRIP"
                                        | "TMP"
                                        | "OBJDUMP"
                                        | "propagatedBuildInputs"
                                        | "CC"
                                        | "__ETC_PROFILE_SOURCED"
                                        | "CONFIG_SHELL"
                                        | "__structuredAttrs"
                                        | "RANLIB"
                                        | "nativeBuildInputs"
                                        | "name"
                                        | "TEST"
                                        | "TZ"
                                        | "HOME"
                                        | "GZIP_NO_TIMESTAMPS"
                                        | "cmakeFlags"
                                        | "TERM"
                                        | "buildCommand"
                                        | "preferLocalBuild"
                                        | "dontAddDisableDepTrack"
                                ))
                        })
                        .filter_map(|var| {
                            Some((var.0.to_string(), var.1.get("value")?.as_str()?.to_owned()))
                        })
                        .map(|(name, value)| {
                            (
                                name,
                                value
                                    .split(':')
                                    .map(|s| s.to_string())
                                    .collect::<Vec<String>>(),
                            )
                        })
                        .collect()
                })
                .context("collecting env vars")?;
            Ok(EnvSet::from_vars(vars))
        } else {
            Err(anyhow!("failed to parse values from JSON output"))
        }
    }
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
