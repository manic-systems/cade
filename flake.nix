{
  inputs = {
    nixpkgs.url = "https://channels.nixos.org/nixpkgs-unstable/nixexprs.tar.xz";
    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    { self, ... }@inputs:
    let
      inherit (inputs) nixpkgs fenix;
      inherit (nixpkgs) lib;
      forAllSystems = lib.genAttrs (lib.systems.doubles.linux ++ lib.systems.doubles.darwin);
      pkgsFor = system: nixpkgs.legacyPackages.${system} or (import nixpkgs { inherit system; });

      # wild + clang are only used on Linux tier-1 arches
      hasWild = plat: plat.isLinux && (plat.isx86_64 || plat.isAarch64);

      rustfmtFor = pkgs: system: fenix.packages.${system}.latest.rustfmt or pkgs.rustfmt;

      nativeDeps =
        pkgs:
        [ pkgs.pkg-config ]
        ++ nixpkgs.lib.optionals (hasWild pkgs.stdenv.hostPlatform) [
          pkgs.wild
          pkgs.clang
        ];

      devPackages =
        pkgs: system:
        [
          pkgs.rustc
          pkgs.cargo
          pkgs.rust-analyzer
          (rustfmtFor pkgs system)
          pkgs.clippy
          pkgs.just
          pkgs.sqlite
        ]
        ++ nativeDeps pkgs;

      testShells = pkgs: [
        pkgs.bashInteractive
        pkgs.zsh
        pkgs.fish
        pkgs.nushell
        pkgs.elvish
        pkgs.murex
      ];
    in
    {
      checks = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.linkFarmFromDrvs "cade-checks" [
            self.checks.${system}.fmt
            self.checks.${system}.clippy
          ];
          fmt =
            pkgs.runCommand "cade-fmt-check"
              {
                nativeBuildInputs = [
                  pkgs.cargo
                  (rustfmtFor pkgs system)
                  pkgs.taplo
                  pkgs.nixfmt
                ];
                src = ./.;
              }
              ''
                cp -r $src ./tree
                chmod -R +w ./tree
                cd ./tree
                cargo fmt -- --check
                taplo fmt --check
                find . -name '*.nix' -exec nixfmt --check {} +
                touch $out
              '';
          clippy = self.packages.${system}.cade.overrideAttrs (old: {
            pname = "cade-clippy";
            nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [ pkgs.clippy ];
            buildPhase = ''
              runHook preBuild
              cargo clippy --all-targets --offline -- -D warnings
              runHook postBuild
            '';
            checkPhase = "true";
            doCheck = false;
            installPhase = ''
              runHook preInstall
              touch $out
              runHook postInstall
            '';
          });
        }
      );
      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.mkShell {
            packages = devPackages pkgs system;
          };

          # Separate from `default` so `load flake` doesn't shadow the user's own
          # interactive shells on PATH. Run the suite with `nix develop .#test`.
          test = pkgs.mkShell {
            packages = devPackages pkgs system ++ testShells pkgs;
          };

          fmt = pkgs.mkShellNoCC {
            packages = [
              pkgs.cargo
              (rustfmtFor pkgs system)
              pkgs.taplo
              pkgs.nixfmt
            ];
            shellHook = ''
              cargo fmt
              taplo fmt
              find . -name '*.nix' -not -path './target/*' -exec nixfmt {} +
            '';
          };
        }
      );
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          cade = pkgs.callPackage ./nix/package.nix { };
          direnvCompat = pkgs.callPackage ./nix/direnv-compat.nix { inherit cade; };
        in
        {
          inherit cade;
          default = cade;
          # cade-backed direnv binary
          direnv-compat = direnvCompat;
        }
      );

      nixosModules.default = import ./nix/module.nix self;
      darwinModules.default = import ./nix/module.nix self;

      # shell init snippets for nushell/elvish/murex
      lib.shellSnippets = import ./nix/snippets.nix { };
    };
}
