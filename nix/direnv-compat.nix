# Builds the cade-backed `direnv` shim from the script beside this file,
# substituting store paths for the @placeholders@.
{
  lib,
  writeScriptBin,
  bash,
  cade,
}:
let
  cadeExe = lib.getExe cade;
in
writeScriptBin "direnv" (
  builtins.replaceStrings [ "@bash@" "@cade@" ] [ "${bash}" cadeExe ] (
    builtins.readFile ./direnv-compat.bash
  )
)
