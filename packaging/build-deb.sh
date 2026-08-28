#!/usr/bin/env bash
set -euo pipefail

if ! command -v cargo-deb >/dev/null 2>&1; then
  echo "cargo-deb is required: cargo install cargo-deb --locked" >&2
  exit 1
fi

export RUSTFLAGS="-C target-cpu=x86-64"
cargo fmt --check
cargo test --locked --features linux-audio
cargo clippy --locked --all-targets --features linux-audio -- -D warnings
cargo build --locked --release --features linux-audio
cargo deb --locked --no-build --output target/debian

shopt -s nullglob
packages=(target/debian/kma-audio-agent_*.deb)
if ((${#packages[@]} != 1)); then
  echo "expected exactly one Debian package in target/debian" >&2
  exit 1
fi

dpkg-deb --info "${packages[0]}"
dpkg-deb --contents "${packages[0]}"
sha256sum "${packages[0]}" > target/debian/SHA256SUMS
printf 'Built %s\n' "${packages[0]}"
