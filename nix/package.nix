# aurora-plugins — the player's external plugins.
#
# The five programs are programs, not a library: `aurora` spawns them and speaks JSON lines. They
# have no dependencies, so the build is offline and the `Cargo.lock` is the only pin.
{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "aurora-plugins";
  version = (lib.importTOML ./../Cargo.toml).workspace.package.version;
  src = ./..;

  cargoLock.lockFile = ./../Cargo.lock;

  # The tests are the protocol's and the plugins' own rules; they need no bus, no network and no
  # daemon, so they run in the build.
  doCheck = true;

  # The player reads a plugin directory: the manifest and the program beside it. The binaries also
  # live in `$out/bin` so a person can try one from a shell.
  postInstall = ''
    for name in notify scrobble mpris remaining autodj; do
      mkdir -p "$out/lib/aurora/plugins/$name"
      install -m 0644 "crates/$name/plugin.json" "$out/lib/aurora/plugins/$name/plugin.json"
      ln -s "$out/bin/aurora-$name" "$out/lib/aurora/plugins/$name/aurora-$name"
    done
  '';

  meta = {
    description = "aurora's external plugins: notify, scrobble, mpris, remaining and autodj";
    mainProgram = "aurora-notify";
  };
}
