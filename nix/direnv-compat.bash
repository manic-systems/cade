#!@bash@/bin/bash
# Minimal direnv shim for tools that call `direnv export json`.
# Shell hook/export probes are no-ops so login-shell env capture keeps working.
set -eu

cade=@cade@

cmd=${1:-}
target=${2:-}

case $cmd in
  export)
    case ${target:-bash} in
      json)
        "$cade" export json
        ;;
      bash | zsh | fish | nushell | nu)
        ;;
      *)
        printf 'direnv shim: unsupported export target: %s\n' "$target" >&2
        exit 1
        ;;
    esac
    ;;
  hook)
    ;;
  *)
    printf 'direnv shim: unsupported command: %s\n' "${cmd:-<empty>}" >&2
    exit 1
    ;;
esac
exit 0
