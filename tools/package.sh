#!/usr/bin/env bash
# Build the AI and lay it out the way Zero-K distributes skirmish AIs:
#   <out>/AI/Skirmish/ZKGBAI/<version>/
#       libSkirmishAI.so   Linux x86_64, linked against glibc 2.17
#       SkirmishAI.dll     Windows x86_64
#       AIInfo.lua, AIOptions.lua
#
# usage: tools/package.sh [-v <version>] [-o <out-dir>] [--native]
#   --native  only build for this machine with plain cargo (no cross
#             toolchain needed; the .so then needs this machine's glibc)
# The cross build needs tools/setup-toolchain.sh to have been run once.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
version=""
out="$root/dist-rs"
native=0
while [ $# -gt 0 ]; do
    case $1 in
        -v) version=$2; shift 2 ;;
        -o) out=$2; shift 2 ;;
        --native) native=1; shift ;;
        *) sed -n '2,12p' "$0"; exit 2 ;;
    esac
done
version=${version:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/crates/zkgbai/Cargo.toml" | head -1)}
ai="$out/AI/Skirmish/ZKGBAI/$version"
mkdir -p "$ai"
cd "$root"

if [ "$native" = 1 ]; then
    cargo build --release -p zkgbai
    cp target/release/libzkgbai.so "$ai/libSkirmishAI.so"
else
    export PATH="$root/target/toolchains/venv/bin:$root/target/toolchains/bin:$PATH"
    command -v cargo-zigbuild > /dev/null || { echo "run tools/setup-toolchain.sh first (or use --native)" >&2; exit 1; }
    cargo zigbuild --release -p zkgbai --target x86_64-unknown-linux-gnu.2.17
    cargo zigbuild --release -p zkgbai --target x86_64-pc-windows-gnu
    cp target/x86_64-unknown-linux-gnu/release/libzkgbai.so "$ai/libSkirmishAI.so"
    cp target/x86_64-pc-windows-gnu/release/zkgbai.dll "$ai/SkirmishAI.dll"
fi

sed "s/@VERSION@/$version/" crates/zkgbai/data/AIInfo.lua > "$ai/AIInfo.lua"
cp crates/zkgbai/data/AIOptions.lua "$ai/AIOptions.lua"
echo "$ai"
