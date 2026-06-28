{
  # nixpkgs, systems, and fenix are pinned with tack (see ./.tack), not flake inputs
  outputs =
    { self, ... }@args:
    let
      inherit (import ./.tack { overrides = args.tackOverrides or { }; }) nixpkgs systems fenix;
      forAllSystems =
        function:
        nixpkgs.lib.genAttrs (import systems) (system: function nixpkgs.legacyPackages.${system} system);
      hasWild =
        pkgs:
        pkgs.stdenv.hostPlatform.isLinux
        && (pkgs.stdenv.hostPlatform.isx86_64 || pkgs.stdenv.hostPlatform.isAarch64);
      nativeDeps =
        pkgs:
        [ pkgs.pkg-config ]
        ++ nixpkgs.lib.optionals (hasWild pkgs) [
          pkgs.wild
          pkgs.clang
        ];
      devPackages =
        pkgs: system:
        [
          pkgs.rustc
          pkgs.cargo
          pkgs.rust-analyzer
          fenix.packages.${system}.latest.rustfmt
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
        pkgs: system: {
          default = pkgs.linkFarmFromDrvs "cade-checks" [
            self.checks.${system}.fmt
            self.checks.${system}.clippy
          ];
          fmt =
            pkgs.runCommand "cade-fmt-check"
              {
                nativeBuildInputs = [
                  pkgs.cargo
                  fenix.packages.${system}.latest.rustfmt
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
        pkgs: system: {
          default = pkgs.mkShell {
            RUSTFLAGS = "-C prefer-dynamic=yes";
            packages = devPackages pkgs system;
          };

          # Separate from `default` so `load flake` doesn't shadow the user's own
          # interactive shells on PATH. Run the suite with `nix develop .#test`.
          test = pkgs.mkShell {
            RUSTFLAGS = "-C prefer-dynamic=yes";
            packages = devPackages pkgs system ++ testShells pkgs;
          };

          fmt = pkgs.mkShellNoCC {
            packages = [
              pkgs.cargo
              fenix.packages.${system}.latest.rustfmt
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
        pkgs: system:
        let
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
