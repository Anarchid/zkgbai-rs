#!/usr/bin/env bash
# Regenerate the raw engine bindings from a Recoil source checkout.
# Only needed when the engine's AI interface headers change.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
recoil="${RECOIL_SRC:-${1:-$HOME/Programs/RecoilEngine}}"
out="$here/../crates/springai-sys/src/bindings.rs"
cargo run --quiet --manifest-path "$here/gen-bindings/Cargo.toml" -- "$recoil" "$out"
echo "engine: $(git -C "$recoil" describe --tags --always 2>/dev/null || echo unknown)"
