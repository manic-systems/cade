{
  lib,
  rustPlatform,
  stdenv,
  pkg-config,
  sqlite,
  clang,
  wild ? null,
}:
let
  manifest = (lib.importTOML ../Cargo.toml).package;
  hasWild =
    stdenv.hostPlatform.isLinux && (stdenv.hostPlatform.isx86_64 || stdenv.hostPlatform.isAarch64);
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

  nativeBuildInputs = [
    pkg-config
  ]
  ++ lib.optionals hasWild [
    wild
    clang
  ];
  buildInputs = [ sqlite ];

  env = lib.optionalAttrs hasWild {
    RUSTFLAGS = "-Clinker=${clang}/bin/clang -Clink-arg=--ld-path=wild";
  };

  meta = {
    description = manifest.description;
    homepage = "https://github.com/manic-systems/cade";
    mainProgram = "cade";
  };
}
