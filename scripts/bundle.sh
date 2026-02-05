#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

echo "Building release binary..."
cargo build --release

BUNDLE_DIR="target/release/bundle/Burrow.app/Contents"

mkdir -p "$BUNDLE_DIR/MacOS"
mkdir -p "$BUNDLE_DIR/Resources"

cp target/release/burrow "$BUNDLE_DIR/MacOS/burrow"
cp resources/Info.plist "$BUNDLE_DIR/Info.plist"

echo "Created target/release/bundle/Burrow.app"
echo ""
echo "To install:"
echo "  cp -r target/release/bundle/Burrow.app /Applications/"
