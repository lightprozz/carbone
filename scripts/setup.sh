#!/bin/sh
set -eu

if [ -f "$HOME/.cargo/env" ]; then
  . "$HOME/.cargo/env"
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo 'Rust 1.93.0 is required. Install the pinned toolchain before running setup (see doc/RUST.md).' >&2
  exit 1
fi
cargo fetch --locked
# Node is used only as the migration's behavior reference, not by the Rust runtime.
npm ci --ignore-scripts
