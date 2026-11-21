#!/usr/bin/env bash
# Build the Rust C FFI crate for iOS targets and produce:
#   ios/TalaDBFfi.xcframework  — used by the podspec (Xcode 12+)
#
# Every slice in the xcframework must use the same library file name
# (libtaladb_ffi.a): CocoaPods refuses to install a vendored xcframework whose
# slices have different binary names.
#
# There is no flat fat .a: the device (arm64) and Apple Silicon simulator
# (arm64) slices share an architecture, so lipo cannot merge them.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RUST_DIR="$PACKAGE_DIR/../ffi"
OUT_DIR="$PACKAGE_DIR/ios"

# Ensure iOS targets are installed
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios

echo "Building for aarch64-apple-ios (device)…"
cargo build --manifest-path "$RUST_DIR/Cargo.toml" \
    --target aarch64-apple-ios --release

echo "Building for aarch64-apple-ios-sim (Apple Silicon simulator)…"
cargo build --manifest-path "$RUST_DIR/Cargo.toml" \
    --target aarch64-apple-ios-sim --release

echo "Building for x86_64-apple-ios (Intel simulator)…"
cargo build --manifest-path "$RUST_DIR/Cargo.toml" \
    --target x86_64-apple-ios --release

CARGO_TARGET="$(cargo metadata --manifest-path "$RUST_DIR/Cargo.toml" \
    --no-deps --format-version 1 | python3 -c \
    'import sys,json; print(json.load(sys.stdin)["target_directory"])')"

# Merge the two simulator slices first. Keep the device slice's file name.
SIM_DIR="$CARGO_TARGET/ios-sim-universal"
mkdir -p "$SIM_DIR"
lipo -create \
    "$CARGO_TARGET/aarch64-apple-ios-sim/release/libtaladb_ffi.a" \
    "$CARGO_TARGET/x86_64-apple-ios/release/libtaladb_ffi.a" \
    -output "$SIM_DIR/libtaladb_ffi.a"

# Create xcframework (device + simulator)
rm -rf "$OUT_DIR/TalaDBFfi.xcframework"
xcodebuild -create-xcframework \
    -library "$CARGO_TARGET/aarch64-apple-ios/release/libtaladb_ffi.a" \
    -headers "$PACKAGE_DIR/cpp" \
    -library "$SIM_DIR/libtaladb_ffi.a" \
    -headers "$PACKAGE_DIR/cpp" \
    -output "$OUT_DIR/TalaDBFfi.xcframework"

echo "Done → $OUT_DIR/TalaDBFfi.xcframework"
