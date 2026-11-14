#!/bin/sh
# Install Palisade's non-NixOS privilege helper. Run with pkexec/sudo.
# Build first: cargo build --release --manifest-path palisade-helper/Cargo.toml
# Optional: pass the absolute path to gpclient as the first argument.
set -eu

src=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo=$(CDPATH= cd -- "$src/../.." && pwd)
target=${CARGO_TARGET_DIR:-$repo/palisade-helper/target}
gpclient_bin=${1:-/usr/bin/gpclient}
openconnect_bin=${2:-/usr/sbin/openconnect}

install -Dm755 "$target/release/palisade-helper" /usr/local/libexec/palisade-helper
install -Dm644 "$src/com.palisade.vpn.policy" /usr/share/polkit-1/actions/com.palisade.vpn.policy
mkdir -p /usr/local/etc
umask 022
printf '{\n  "gpclient_bin": "%s",\n  "openconnect_bin": "%s"\n}\n' \
    "$gpclient_bin" "$openconnect_bin" > /usr/local/etc/palisade-helper.json
chmod 644 /usr/local/etc/palisade-helper.json

echo 'Installed the Palisade privileged helper, config and polkit policy'
echo 'Now run install-user-wrappers.sh as the desktop user'
