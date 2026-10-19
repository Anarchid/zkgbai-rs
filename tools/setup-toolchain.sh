#!/usr/bin/env bash
# Install the cross-build toolchain for release packaging into .toolchains
# (project-local; delete that directory to remove it):
#   - zig (as a cross linker), via the `ziglang` Python package in a venv
#   - cargo-zigbuild
#   - the Rust standard library for x86_64-pc-windows-gnu
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
tc="$root/.toolchains"
mkdir -p "$tc"
[ -x "$tc/venv/bin/python" ] || python3 -m venv "$tc/venv"
"$tc/venv/bin/pip" install -q ziglang
cargo install -q cargo-zigbuild --root "$tc"
rustup target add x86_64-pc-windows-gnu
echo "toolchain ready in $tc"
