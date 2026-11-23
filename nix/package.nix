{ lib
, pkgs
, rustPlatform
, pkg-config
, openssl
, dbus
, makeWrapper
, libGL
, libxkbcommon
, wayland
, libx11
, libxcursor
, libxrandr
, libxi
, libxext
, fontconfig
, freetype
, python3
, curl
, openconnect
}:

rustPlatform.buildRustPackage rec {
  pname = "palisade";
  version = "0.1.0";

  src = lib.cleanSourceWith {
    src = ../.;
    filter = path: type:
      let rel = lib.removePrefix (toString ../. + "/") (toString path);
      in !lib.any (p: lib.hasPrefix p rel) [ "target" "win_mac_binaries" ".git" ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  # skia-bindings cannot download its prebuilt archive from the Nix sandbox,
  # and a source build would need to vendor Skia's depot_tools. The prebuilt
  # archive is fetched as a fixed-output derivation instead and exposed to the
  # build script through its documented SKIA_BINARIES_URL override.
  skiaBinaries = pkgs.fetchurl {
    url = "https://github.com/rust-skia/skia-binaries/releases/download/0.78.2/skia-binaries-ec00cf219c4901d785ed-x86_64-unknown-linux-gnu-gl-textlayout.tar.gz";
    hash = "sha256-HOIN5V03GDZoAVTTMETLUhLBVY19w3Otbc+oYfl4WZY=";
  };

  nativeBuildInputs = [ pkg-config makeWrapper python3 ];
  SKIA_BINARIES_URL = "file://${skiaBinaries}";
  buildInputs = [ openssl dbus.dev fontconfig.dev freetype.dev ];

  runtimeLibs = lib.makeLibraryPath [
    libGL
    libxkbcommon
    wayland
    libx11
    libxcursor
    libxrandr
    libxi
    libxext
    fontconfig
    freetype
  ];

  postInstall = ''
    wrapProgram $out/bin/palisade \
      --prefix PATH : ${lib.makeBinPath [ openconnect ]} \
      --prefix LD_LIBRARY_PATH : $runtimeLibs

    mkdir -p $out/share/applications
    substitute ${./palisade.desktop} $out/share/applications/palisade.desktop \
      --replace-fail @BIN@ $out/bin/palisade
  '';

  meta = {
    description = "Open-source GUI client for Palo Alto GlobalProtect VPN (wraps openconnect)";
    homepage = "https://github.com/varsoug/Palisade";
    license = lib.licenses.gpl3Plus;
    mainProgram = "palisade";
    platforms = lib.platforms.linux ++ lib.platforms.darwin;
  };
}
