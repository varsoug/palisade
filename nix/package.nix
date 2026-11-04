{ lib
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
, openconnect
}:

rustPlatform.buildRustPackage rec {
  pname = "gpconnect";
  version = "0.1.0";

  src = lib.cleanSourceWith {
    src = ../.;
    filter = path: type:
      let rel = lib.removePrefix (toString ../. + "/") (toString path);
      in !lib.any (p: lib.hasPrefix p rel) [ "target" "win_mac_binaries" ".git" ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [ pkg-config makeWrapper ];
  buildInputs = [ openssl dbus.dev ];

  runtimeLibs = lib.makeLibraryPath [
    libGL
    libxkbcommon
    wayland
    libx11
    libxcursor
    libxrandr
    libxi
    libxext
  ];

  postInstall = ''
    wrapProgram $out/bin/gpconnect \
      --prefix PATH : ${lib.makeBinPath [ openconnect ]} \
      --prefix LD_LIBRARY_PATH : $runtimeLibs
  '';

  meta = {
    description = "Open-source GUI client for Palo Alto GlobalProtect VPN (wraps openconnect)";
    homepage = "https://github.com/varsoug/GPConnect";
    license = lib.licenses.gpl3Plus;
    mainProgram = "gpconnect";
    platforms = lib.platforms.linux ++ lib.platforms.darwin;
  };
}
