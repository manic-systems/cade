self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.cade;
  exe = lib.getExe cfg.package;
  snippets = import ./snippets.nix;
in
{
  options.programs.cade = {
    enable = lib.mkEnableOption "an intelligent, cascading environment manager";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage "${self}/nix/package.nix" { };
      defaultText = lib.literalExpression "cade built from the cade flake";
      description = "The cade package to install and hook into shells.";
    };

    enableBashIntegration = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Add the cade hook to interactive bash sessions.";
    };

    enableZshIntegration = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Add the cade hook to interactive zsh sessions.";
    };

    enableFishIntegration = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Add the cade hook to interactive fish sessions.";
    };

    shellSnippets = {
      nushell = lib.mkOption {
        type = lib.types.lines;
        readOnly = true;
        default = snippets.nushell;
        description = "Init snippet enabling cade in Nushell (add to config.nu).";
      };

      elvish = lib.mkOption {
        type = lib.types.lines;
        readOnly = true;
        default = snippets.elvish;
        description = "Init snippet enabling cade in Elvish (add to rc.elv).";
      };

      murex = lib.mkOption {
        type = lib.types.lines;
        readOnly = true;
        default = snippets.murex;
        description = "Init snippet enabling cade in Murex (add to ~/.murex_profile).";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    programs.bash.interactiveShellInit = lib.mkIf cfg.enableBashIntegration ''
      eval "$(${exe} hook bash)"
    '';
    programs.zsh.interactiveShellInit = lib.mkIf cfg.enableZshIntegration ''
      eval "$(${exe} hook zsh)"
    '';
    programs.fish.interactiveShellInit = lib.mkIf cfg.enableFishIntegration ''
      ${exe} hook fish | source
    '';
  };
}
