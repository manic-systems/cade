# Init snippets for the shells with no system-level interactive-init hook on
# NixOS / nix-darwin (nushell, elvish, murex). By default these invoke `cade`
# from PATH, so this needs no package/pkgs. The module passes an escaped command
# with its configured package/config file.
{
  cade ? "cade",
}:
{
  nushell = ''
    mkdir ~/.cache/cade
    ${cade} hook nushell | save --force ~/.cache/cade/hook.nu
    source ~/.cache/cade/hook.nu
  '';
  elvish = "eval (${cade} hook elvish | slurp)\n";
  murex = "${cade} hook murex -> source\n";
}
