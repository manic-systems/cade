{
  lib,
  writeScriptBin,
  bash,
  cade,
  mode ? "full",
}:
let
  cadeExe = lib.getExe cade;
in
assert lib.assertOneOf "mode" mode [
  "shim"
  "full"
];
writeScriptBin "direnv" ''
  #!${bash}/bin/bash
  # Minimal direnv shim for tools that call `direnv export json`.
  # Shell hook/export probes are no-ops so login-shell env capture keeps working.
  set -eu

  cade=${lib.escapeShellArg cadeExe}
  cade_direnv_mode=${lib.escapeShellArg mode}

  cmd=''${1:-}
  target=''${2:-}

  case $cmd in
    export)
      case ''${target:-bash} in
        json)
          CADE_DIRENV=''${CADE_DIRENV:-$cade_direnv_mode} "$cade" export json
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
      printf 'direnv shim: unsupported command: %s\n' "''${cmd:-<empty>}" >&2
      exit 1
      ;;
  esac
  exit 0
''
