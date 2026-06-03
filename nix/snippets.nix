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
