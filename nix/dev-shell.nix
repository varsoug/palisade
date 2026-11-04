{ lib
, mkShell
, cargo
, rustc
, clippy
, rustfmt
, pkg-config
, openssl
, dbus
, libGL
, libxkbcommon
, wayland
, xorg
, openconnect
}:

mkShell {
  packages = [ cargo rustc clippy rustfmt pkg-config openssl dbus.dev openconnect ];

  LD_LIBRARY_PATH = lib.makeLibraryPath [
    libGL
    libxkbcommon
    wayland
    xorg.libX11
    xorg.libXcursor
    xorg.libXrandr
    xorg.libXi
    xorg.libXext
  ];

  shellHook = ''
    echo "gpconnect dev shell — cargo build / cargo clippy --all-targets -- -D warnings / cargo test"
  '';
}
