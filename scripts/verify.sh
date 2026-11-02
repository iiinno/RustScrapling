#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
if [[ -x "$HOME/.cargo/bin/rustc" ]]; then
  # rustdoc invokes rustc internally: ensure they use the same toolchain.
  export PATH="$HOME/.cargo/bin:$PATH"
fi

echo "rustc: $(rustc --version)"
echo "cargo: $(cargo --version)"
pkg-config --modversion libxml-2.0

cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
