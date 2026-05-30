{
  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
  inputs.systems.url = "github:nix-systems/default-linux";

  outputs =
    {
      self,
      nixpkgs,
      systems,
    }:
    let
      forAllSystems =
        function: nixpkgs.lib.genAttrs (import systems) (system: function nixpkgs.legacyPackages.${system});
    in
    {
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          RUSTFLAGS = "-C prefer-dynamic=yes";
          packages = builtins.attrValues {
            inherit (pkgs)
              rustc
              cargo
              rust-analyzer
              rustfmt
              clippy
              just
              sqlite
              ;
          };
        };
      });
      packages = forAllSystems (
        pkgs:
        let
          cade = pkgs.callPackage ./nix/package.nix { };
          direnvShims = pkgs.callPackage ./nix/direnv-compat.nix { inherit cade; };
        in
        {
          inherit cade;
          default = cade;
          # cade-backed `direnv` binaries; also wired into the module behind
          # programs.cade.direnvCompat
          direnv-compat-bash = direnvShims.bash;
          direnv-compat-nu = direnvShims.nu;
        }
      );

      # System modules add the cade package and wire its shell hooks into
      # interactive bash/zsh/fish. The same module works on both platforms.
      #
      #   programs.cade.enable = true;
      #
      # nushell/elvish/murex have no system-level init hook on NixOS or
      # nix-darwin; for those, add `cade hook <shell>` to the user's shell
      # config (see the README) or use a home-manager setup.
      nixosModules.default = import ./nix/module.nix self;
      darwinModules.default = import ./nix/module.nix self;

      # Shell init snippets for nushell/elvish/murex (plain strings invoking
      # `cade` from PATH), accessible without evaluating the system module:
      #   cade.lib.shellSnippets.nushell
      lib.shellSnippets = import ./nix/snippets.nix { };
    };
}
