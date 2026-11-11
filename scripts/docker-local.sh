#!/usr/bin/env bash
# Build the Linux binary on this machine (all cores, incremental) and package it as vigily:local,
# instead of compiling inside Docker. Needs cargo-zigbuild and zig, so the binary runs on the image's
# Debian bookworm (glibc 2.36) whatever this machine's glibc is.
set -euo pipefail
cd "$(dirname "$0")/.."

arch=$(uname -m)
case "$arch" in
  arm64 | aarch64) target=aarch64-unknown-linux-gnu docker_arch=arm64 ;;
  x86_64) target=x86_64-unknown-linux-gnu docker_arch=amd64 ;;
  *) echo "unsupported architecture: $arch" >&2; exit 1 ;;
esac

if ! command -v cargo-zigbuild >/dev/null; then
  echo "cargo-zigbuild not found: cargo install cargo-zigbuild, and install zig" >&2
  exit 1
fi

(cd crates/vigily-ui && npm ci --silent --no-audit --no-fund && npm run build --silent)
CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
  cargo zigbuild --release --locked --target "$target.2.36" -p vigily-cli \
  --features "${FEATURES-s3,gcs,azure}" -j "${JOBS:-6}"
mkdir -p "dist/linux/$docker_arch"
cp "target/$target/release/vigily" "dist/linux/$docker_arch/vigily"
docker build --target local -t vigily:local .
echo "vigily:local is ready: VIGILY_IMAGE=vigily:local docker compose up -d"
