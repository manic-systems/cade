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
  # direnvCompat accepts the legacy boolean (pre-enum releases) for back-compat:
  # `true` mapped to installing the shim, `false` to doing nothing. Normalize to
  # the string form everything else consumes, and nudge boolean users onward.
  direnvCompatString =
    if builtins.isBool cfg.direnvCompat then
      (if cfg.direnvCompat then "shim" else "none")
    else
      cfg.direnvCompat;
  configValues = lib.filterAttrs (_: v: v != null) {
    inherit (cfg) verbosity;
    long_running_warning_ms = cfg.longRunningWarningMs;
    shell_gc_root_ttl_seconds = cfg.shellGcRootTtlSeconds;
    # configFile owns the whole config, so don't inject direnv when it's set;
    # otherwise "envrc" is the cade default and needs no explicit write.
    direnv =
      if cfg.configFile != null || direnvCompatString == "envrc" then null else direnvCompatString;
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
  cadeCmd = lib.escapeShellArgs (
    map toString (
      [ exe ]
      ++ lib.optionals (activeConfigFile != null) [
        "--config"
        activeConfigFile
      ]
    )
  );
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
      type = lib.types.nullOr (
        lib.types.enum [
          "quiet"
          "normal"
          "vars"
          "trace"
        ]
      );
      default = null;
      description = "Default diagnostic verbosity written to cade's generated TOML config.";
    };

    longRunningWarningMs = lib.mkOption {
      type = lib.types.nullOr lib.types.ints.positive;
      default = null;
      description = "External loader warning threshold, in milliseconds.";
    };

    shellGcRootTtlSeconds = lib.mkOption {
      type = lib.types.nullOr lib.types.ints.positive;
      default = null;
      description = "Shell GC root and snapshot retention time, in seconds.";
    };

    configFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "Strict TOML config path passed to cade with --config instead of generating one from module options.";
    };

    direnvCompat = lib.mkOption {
      # the enum is hand-mirrored by the Rust DirenvMode enum (src/config.rs);
      # keep the two in sync. `bool` is accepted for back-compat with the
      # pre-enum option (legacy `true` -> "shim", `false` -> "none"); it is
      # deprecated and warns. Drop the bool arm after a release.
      type =
        with lib.types;
        either bool (enum [
          "none"
          "shim"
          "envrc"
          "full"
        ]);
      # stay out of the way when real direnv is hooked into the shell, otherwise
      # read .envrc ourselves. programs.direnv.enable is the right signal: it
      # means direnv is hooked into the shell, the thing that conflicts. we don't
      # read systemPackages here because this module conditionally adds its own
      # shim to it based on direnvCompat, so the default would recurse on the
      # option it's defining (nix infinite-recursion).
      default = if (config.programs.direnv.enable or false) then "none" else "envrc";
      example = "full";
      description = ''
        Which direnv compatibility cade enables, written to its config as
        `direnv`. Defaults to `none` when `programs.direnv.enable` is set
        (let real direnv own `.envrc`), otherwise `envrc`.

        - `none`: neither the implicit `.envrc` loader nor the export shim.
        - `shim`: install the cade-backed `direnv` shim; the implicit `.envrc`
          loader stays off.
        - `envrc`: cade implicitly loads a bare `.envrc`; no shim.
        - `full`: both the implicit `.envrc` loader and the shim.

        The shim is the cade-backed `direnv` executable on PATH for editor and
        tool compatibility. This is not the shell integration path; interactive
        shells should use cade's native hook snippets. The shim collides with a
        real direnv in environment.systemPackages, so install only one.

        Deprecated: a boolean is still accepted from the pre-enum option
        (`true` behaves as `"shim"`, `false` as `"none"`) but warns. Use the
        string form. Leaving the option unset keeps the string default above
        rather than any boolean.
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
        message = "programs.cade.configFile cannot be combined with generated config options.";
      }
      {
        # configFile owns the whole runtime config, so the generated `direnv`
        # value is suppressed (see configValues above). But the shim package is
        # installed independently from direnvCompat, so "shim"/"full" would
        # still drop the shim on PATH while silently not affecting runtime mode
        # (split-brain). Reject the ambiguous combination; configFile users
        # should set `direnv` inside their TOML and leave direnvCompat unset, or
        # set it explicitly to the default-equivalent string for clarity.
        assertion =
          cfg.configFile == null
          || builtins.elem direnvCompatString [
            "none"
            "envrc"
          ];
        message = ''
          programs.cade.direnvCompat = "${direnvCompatString}" is ignored at
          runtime when programs.cade.configFile is set (configFile owns the
          config), yet it still installs the direnv shim. Put the `direnv` key
          in your configFile TOML instead, and leave direnvCompat unset.
        '';
      }
    ];

    warnings = lib.optional (builtins.isBool cfg.direnvCompat) ''
      programs.cade.direnvCompat is set to a boolean (${lib.boolToString cfg.direnvCompat}).
      The boolean form is deprecated; it is read as "${direnvCompatString}" for now.
      Set it to one of "none", "shim", "envrc", or "full" instead.
    '';

    environment.systemPackages = [
      cfg.package
    ]
    ++ lib.optional (direnvCompatString == "shim" || direnvCompatString == "full") direnvShim;

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
