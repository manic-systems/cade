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
