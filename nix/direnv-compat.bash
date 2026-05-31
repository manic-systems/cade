#!@bash@/bin/bash
# direnv shim: maps the direnv cli surface tools rely on onto cade calls.
# drop on PATH as `direnv`. unknown subcommands are silent no-ops
set -u

cade=@cade@

cmd=${1:-}
target=${2:-}

target_dir() {
  local operand=${1:-}

  if [ -z "$operand" ]; then
    printf '.\n'
  elif [ -d "$operand" ]; then
    printf '%s\n' "$operand"
  else
    dirname -- "$operand"
  fi
}

run_in_target_dir() {
  local action=$1
  local operand=${2:-}
  local dir

  dir=$(target_dir "$operand") || return
  (
    cd -- "$dir" && "$cade" "$action"
  )
}

case $cmd in
  export)
    case ${target:-bash} in
      json)
        out=$("$cade" reload --shell json)
        status=$?
        if [ "$status" -ne 0 ]; then
          [ -z "$out" ] || printf '%s\n' "$out"
          exit "$status"
        fi
        [ -n "$out" ] || out='{}'
        printf '%s\n' "$out"
        ;;
      bash | zsh | fish | nushell | nu)
        "$cade" reload --shell "${target:-bash}"
        ;;
    esac
    ;;
  hook)
    "$cade" hook "${target:-bash}"
    ;;
  allow | permit | grant)
    run_in_target_dir allow "$target"
    ;;
  deny | block | revoke)
    run_in_target_dir disallow "$target"
    ;;
  status)
    if [ "${target:-}" = "--json" ]; then
      printf 'direnv shim: status --json is unsupported\n' >&2
      exit 1
    fi
    "$cade" status
    ;;
  version)
    # satisfy version-gated callers
    echo "2.34.0"
    ;;
esac
