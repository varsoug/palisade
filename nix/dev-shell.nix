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
, libx11
, libxcursor
, libxrandr
, libxi
, libxext
, openconnect
}:

mkShell {
  packages = [ cargo rustc clippy rustfmt pkg-config openssl dbus.dev openconnect ];

  LD_LIBRARY_PATH = lib.makeLibraryPath [
    libGL
    libxkbcommon
    wayland
    libx11
    libxcursor
    libxrandr
    libxi
    libxext
  ];

  shellHook = ''
    echo "palisade dev shell — cargo build / cargo clippy --all-targets -- -D warnings / cargo test"
  '';
}
