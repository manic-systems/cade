# Shared NixOS / nix-darwin module for cade. Both platforms expose the same
# `programs.{bash,zsh,fish}.interactiveShellInit` options, so a single module
# serves both. Wired up in flake.nix as nixosModules.default and
# darwinModules.default.
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
  configValues = lib.filterAttrs (_: v: v != null) {
    inherit (cfg) verbosity;
    long_running_warning_ms = cfg.longRunningWarningMs;
  };
  tomlFormat = pkgs.formats.toml { };
  generatedConfigFile = tomlFormat.generate "cade-config.toml" configValues;
  activeConfigFile =
    if cfg.configFile != null then
      cfg.configFile
    else if configValues != { } then
      generatedConfigFile
    else
      null;
  cadeCmd = lib.escapeShellArgs (map toString (
    [ exe ] ++ lib.optionals (activeConfigFile != null) [
      "--config"
      activeConfigFile
    ]
  ));
  snippets = import ./snippets.nix { cade = cadeCmd; };
  direnvShim = pkgs.callPackage "${self}/nix/direnv-compat.nix" { cade = cfg.package; };
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

    verbosity = lib.mkOption {
      type = lib.types.nullOr (lib.types.enum [
        "quiet"
        "normal"
        "vars"
        "trace"
      ]);
      default = null;
      description = "Default diagnostic verbosity written to cade's generated TOML config.";
    };

    longRunningWarningMs = lib.mkOption {
      type = lib.types.nullOr lib.types.ints.positive;
      default = null;
      description = "External loader warning threshold, in milliseconds.";
    };

    configFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "Strict TOML config path passed to cade with --config instead of generating one from module options.";
    };

    direnvCompat = lib.mkOption {
      type = lib.types.bool;
      default = false;
      example = true;
      description = ''
        Install a cade-backed `direnv` executable on PATH for editor and tool
        compatibility. This is not the shell integration path; interactive
        shells should use cade's native hook snippets. The shim collides with a
        real direnv in environment.systemPackages, so install only one.
      '';
    };

    # Nushell, Elvish, and Murex have no system-level interactive-init hook on
    # NixOS or nix-darwin, so the module can't wire them automatically. These
    # read-only snippets expose the exact init lines for each, so you can drop
    # them into your own config, e.g.
    #   programs.nushell.extraConfig = config.programs.cade.shellSnippets.nushell;
    # (home-manager), or write them to the relevant rc file.
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
    assertions = [
      {
        assertion = cfg.configFile == null || configValues == { };
        message = "programs.cade.configFile cannot be combined with programs.cade.verbosity or programs.cade.longRunningWarningMs.";
      }
    ];

    environment.systemPackages = [ cfg.package ] ++ lib.optional cfg.direnvCompat direnvShim;

    # bash and zsh evaluate the hook; the shell flag must be enabled by the user
    # (programs.zsh.enable / shell installed) for the init file to be sourced.
    programs.bash.interactiveShellInit = lib.mkIf cfg.enableBashIntegration ''
      eval "$(${cadeCmd} hook bash)"
    '';
    programs.zsh.interactiveShellInit = lib.mkIf cfg.enableZshIntegration ''
      eval "$(${cadeCmd} hook zsh)"
    '';
    # fish sources the hook directly rather than via eval
    programs.fish.interactiveShellInit = lib.mkIf cfg.enableFishIntegration ''
      ${cadeCmd} hook fish | source
    '';
  };
}
