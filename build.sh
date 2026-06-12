#!/usr/bin/env bash
set -euo pipefail

# One-time setup:
#   rustup target add aarch64-apple-darwin x86_64-apple-darwin \
#       x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-pc-windows-gnu
#   brew install zig cmake nasm
#   cargo install --locked cargo-zigbuild

APP_NAME="grindr-always-online"
DIST_DIR="$(pwd)/dist"

HOST_TRIPLE="$(rustc -vV | awk '/^host:/{print $2}')"
BORING_ZIG_ENV=("CMAKE_TOOLCHAIN_FILE_${HOST_TRIPLE//-/_}=1")

mkdir -p "$DIST_DIR"

package() { # package <binary-path> <zip-name>
	local bin="$1" zipname="$2" staging
	staging="$(mktemp -d)"
	cp "$bin" "$staging/"
	rm -f "$DIST_DIR/$zipname"
	(cd "$staging" && zip -q "$DIST_DIR/$zipname" ./*)
	rm -rf "$staging"
	echo "✔ dist/$zipname"
}

echo "→ macOS arm64"
cargo build --release --target aarch64-apple-darwin

echo "→ macOS x86_64"
cargo build --release --target x86_64-apple-darwin

echo "→ macOS universal (lipo)"
mkdir -p target/universal-apple-darwin/release
lipo -create \
	"target/aarch64-apple-darwin/release/$APP_NAME" \
	"target/x86_64-apple-darwin/release/$APP_NAME" \
	-output "target/universal-apple-darwin/release/$APP_NAME"
package "target/universal-apple-darwin/release/$APP_NAME" macos-universal.zip

echo "→ Linux x86_64 (static, musl)"
env "${BORING_ZIG_ENV[@]}" cargo zigbuild --release --target x86_64-unknown-linux-musl
package "target/x86_64-unknown-linux-musl/release/$APP_NAME" linux-x86_64.zip

echo "→ Linux arm64 (static, musl)"
env "${BORING_ZIG_ENV[@]}" cargo zigbuild --release --target aarch64-unknown-linux-musl
package "target/aarch64-unknown-linux-musl/release/$APP_NAME" linux-arm64.zip

echo "→ Windows x86_64"
env "${BORING_ZIG_ENV[@]}" cargo zigbuild --release --target x86_64-pc-windows-gnu
package "target/x86_64-pc-windows-gnu/release/$APP_NAME.exe" windows-x86_64.zip

echo
echo "ALL BUILDS COMPLETE"
ls -lah "$DIST_DIR"

echo
echo "Verification:"
file "target/universal-apple-darwin/release/$APP_NAME"
file "target/x86_64-unknown-linux-musl/release/$APP_NAME"
file "target/aarch64-unknown-linux-musl/release/$APP_NAME"
file "target/x86_64-pc-windows-gnu/release/$APP_NAME.exe"
