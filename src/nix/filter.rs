const IGNORED_ENV_PREFIXES: &[&str] = &["NIX_", "output", "deps", "enable"];
const IGNORED_ENV_SUFFIXES: &[&str] = &["Inputs", "Flags", "TYPE"];
const KEPT_NIX_ENV_KEYS: &[&str] = &[
    "NIX_BINTOOLS",
    "NIX_CC",
    "NIX_CFLAGS_COMPILE",
    "NIX_ENFORCE_NO_NATIVE",
    "NIX_HARDENING_ENABLE",
    "NIX_LDFLAGS",
    "NIX_STORE",
];
const KEPT_NIX_ENV_PREFIXES: &[&str] = &[
    "NIX_BINTOOLS_WRAPPER_TARGET_",
    "NIX_CC_WRAPPER_TARGET_",
    "NIX_PKG_CONFIG_WRAPPER_TARGET_",
];
const IGNORED_ENV_KEYS: &[&str] = &[
    "SHELL",
    "pkg",
    "prefix",
    "guess",
    "_substituteStream_has_warned_replace_deprecation",
    "LINENO",
    "OPTERROR",
    "OLDPWD",
    "BASH",
    "IFS",
    "PS4",
    "initialPath",
    "out",
    "shell",
    "STRINGS",
    "stdenv",
    "builder",
    "PWD",
    "SOURCE_DATE_EPOCH",
    "CXX",
    "TEMPDIR",
    "system",
    "HOST_PATH",
    "doInstallCheck",
    "buildCommandPath",
    "LS_COLORS",
    "cmakeFlakes",
    "TMPDIR",
    "LD",
    "READELF",
    "doCheck",
    "SIZE",
    "propagatedNativeBuildInputs",
    "strictDeps",
    "AR",
    "AS",
    "TEMP",
    "SHLVL",
    "NM",
    "patches",
    "passAsFile",
    "buildInputs",
    "SSL_CERT_FILE",
    "OBJCOPY",
    "STRIP",
    "TMP",
    "OBJDUMP",
    "propagatedBuildInputs",
    "CC",
    "__ETC_PROFILE_SOURCED",
    "CONFIG_SHELL",
    "__structuredAttrs",
    "RANLIB",
    "nativeBuildInputs",
    "name",
    "TEST",
    "TZ",
    "HOME",
    "GZIP_NO_TIMESTAMPS",
    "cmakeFlags",
    "TERM",
    "buildCommand",
    "preferLocalBuild",
    "dontAddDisableDepTrack",
];

pub(super) fn keep_loaded_env_var(var: &str) -> bool {
    if is_kept_nix_env_var(var) {
        return true;
    }

    !(IGNORED_ENV_PREFIXES
        .iter()
        .any(|prefix| var.starts_with(prefix))
        || IGNORED_ENV_SUFFIXES
            .iter()
            .any(|suffix| var.ends_with(suffix))
        || var.to_lowercase().contains("phase")
        || IGNORED_ENV_KEYS.contains(&var))
}

pub(super) fn is_kept_nix_env_var(var: &str) -> bool {
    KEPT_NIX_ENV_KEYS.contains(&var)
        || KEPT_NIX_ENV_PREFIXES
            .iter()
            .any(|prefix| var.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_nix_wrapper_environment() {
        for key in [
            "NIX_BINTOOLS",
            "NIX_BINTOOLS_WRAPPER_TARGET_HOST_x86_64_unknown_linux_gnu",
            "NIX_CC",
            "NIX_CC_WRAPPER_TARGET_HOST_x86_64_unknown_linux_gnu",
            "NIX_CFLAGS_COMPILE",
            "NIX_ENFORCE_NO_NATIVE",
            "NIX_HARDENING_ENABLE",
            "NIX_LDFLAGS",
            "NIX_PKG_CONFIG_WRAPPER_TARGET_HOST_x86_64_unknown_linux_gnu",
            "NIX_STORE",
        ] {
            assert!(keep_loaded_env_var(key), "{key} should be kept");
        }
    }

    #[test]
    fn still_filters_noisy_nix_internals() {
        for key in ["NIX_BUILD_TOP", "NIX_GCROOT", "NIX_PROFILES", "NIX_PATH"] {
            assert!(!keep_loaded_env_var(key), "{key} should stay filtered");
        }
    }
}
