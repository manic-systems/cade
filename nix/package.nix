{
  lib,
  rustPlatform,
  sqlite,
}:
let
  manifest = (lib.importTOML ../Cargo.toml).package;
in
rustPlatform.buildRustPackage {
  pname = manifest.name;
  version = manifest.version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../src
      ../tests
      ../nix/direnv-compat.bash
      ../Cargo.toml
      ../Cargo.lock
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  buildInputs = [ sqlite ];

  meta = {
    description = manifest.description;
    homepage = "https://github.com/manic-systems/cade";
    mainProgram = "cade";
  };
}
