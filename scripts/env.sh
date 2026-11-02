# RustScrapling build environment for the Muse VM (Plan section 4).
# Usage: source scripts/env.sh
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR=/home/hatch/rustscrapling-target
export TMPDIR=/home/hatch/tmp
export CARGO_BUILD_JOBS=2
# libclang for bindgen-based crates (wreq's btls-sys). Installed via apt
# (llvm 18). The libxml crate does not need it thanks to the LIBXML2 env
# route in DECISIONS.md D-004.
export LIBCLANG_PATH=/usr/lib/llvm-18/lib
# Static libxml2 2.14.6 (DECISIONS.md D-004)
export LIBXML2=/home/hatch/libxml2/lib/libxml2.a
export PKG_CONFIG_PATH=/home/hatch/libxml2/lib/pkgconfig
export CHROME_PATH=/home/hatch/chrome-for-testing/chrome-linux64/chrome
mkdir -p "$TMPDIR" "$CARGO_TARGET_DIR"
