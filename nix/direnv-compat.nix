# builds the `direnv` shims from the scripts beside this file, substituting
# store paths for the @placeholders@. both just relay `cade reload`, so they
# behave identically; pick by which interpreter you'd rather depend on
{
  lib,
  writeScriptBin,
  bash,
  nushell,
  cade,
}:
let
  cadeExe = lib.getExe cade;
  fromTemplate =
    replacements: file:
    writeScriptBin "direnv" (
      builtins.replaceStrings (builtins.attrNames replacements) (builtins.attrValues replacements) (
        builtins.readFile file
      )
    );
in
{
  bash = fromTemplate {
    "@bash@" = "${bash}";
    "@cade@" = cadeExe;
  } ./direnv-compat.bash;

  nu = fromTemplate {
    "@nu@" = "${nushell}";
    "@cade@" = cadeExe;
  } ./direnv-compat.nu;
}
