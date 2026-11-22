{
  lib,
  rustPlatform,
  stdenv,
  pkg-config,
  sqlite,
  buildPackages,
  installShellFiles,
}:
let
  inherit (buildPackages) clang wild;
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
      ../Cargo.toml
      ../Cargo.lock
      ../README.md
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [
    installShellFiles
    pkg-config
  ]
  ++ lib.optionals hasWild [
    wild
    clang
  ];
  buildInputs = [ sqlite ];

  stripAllList = [ "bin" ];

  env = lib.optionalAttrs hasWild {
    RUSTFLAGS = "-Clinker=${clang}/bin/${clang.targetPrefix}clang -Clink-arg=--ld-path=wild";
  };

  postInstall = lib.optionalString (stdenv.hostPlatform.emulatorAvailable buildPackages) (
    let
      cade = "${stdenv.hostPlatform.emulator buildPackages} $out/bin/cade";
    in
    ''
      installShellCompletion --cmd cade \
        --bash    <(COMPLETE=bash ${cade}) \
        --zsh     <(COMPLETE=zsh  ${cade}) \
        --fish    <(COMPLETE=fish ${cade}) \
        --nushell <(COMPLETE=nu   ${cade})
    ''
  );

  meta = {
    description = manifest.description;
    homepage = "https://github.com/manic-systems/cade";
    mainProgram = "cade";
  };
}
