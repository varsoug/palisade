#!/bin/sh
# Install Palisade's per-user helper client and backend wrappers.
# Build first: cargo build --release --manifest-path palisade-helper/Cargo.toml
set -eu

src=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo=$(CDPATH= cd -- "$src/../.." && pwd)
target=${CARGO_TARGET_DIR:-$repo/palisade-helper/target}
bin=${HOME}/.local/bin
install -Dm755 "$target/release/palisade-helper-client" "$bin/palisade-helper-client"
install -Dm755 "$src/palisade-gpclient-wrapper" "$bin/gpclient"
install -Dm755 "$src/palisade-openconnect-wrapper" "$bin/palisade-openconnect"

echo "Installed Palisade helper client and wrappers in $bin"
