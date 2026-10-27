#!/bin/bash
# Build the macOS installer: dist/LBP2900-Driver-<version>.pkg
# Runs on macOS (GitHub Actions macos-14 or a Mac with Rust installed).
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
BINS=(rastertocapt lbp2900 lbp2900-app)
ID=io.github.mk-manishkandel.lbp2900
APP="LBP2900 Driver.app"
export MACOSX_DEPLOYMENT_TARGET=11.0

for t in "${TARGETS[@]}"; do
  cargo build --release --locked --target "$t" -p rastertocapt -p lbp2900-backend -p lbp2900-app
done

rm -rf dist && mkdir -p dist/bin
for b in "${BINS[@]}"; do
  lipo -create -output "dist/bin/$b" \
    "target/${TARGETS[0]}/release/$b" "target/${TARGETS[1]}/release/$b"
  codesign --force --sign - "dist/bin/$b"
done

ROOT=dist/root
SUPPORT="$ROOT/Library/Printers/LBP2900"
mkdir -p "$ROOT/usr/libexec/cups/backend" "$SUPPORT/filter" \
  "$ROOT/Library/Printers/PPDs/Contents/Resources" "$ROOT/Library/LaunchAgents" \
  "$ROOT/Applications/$APP/Contents/MacOS"

install -m 0700 dist/bin/lbp2900 "$ROOT/usr/libexec/cups/backend/lbp2900"
install -m 0755 dist/bin/rastertocapt "$SUPPORT/filter/rastertocapt"
install -m 0755 packaging/setup-queue.sh "$SUPPORT/setup-queue.sh"
install -m 0755 packaging/uninstall.sh "$SUPPORT/uninstall.sh"
install -m 0644 packaging/lbp2900.conf.example "$SUPPORT/lbp2900.conf.example"
gzip -9 -c ppd/Canon-LBP2900-Rust.ppd > "$ROOT/Library/Printers/PPDs/Contents/Resources/Canon-LBP2900-Rust.ppd.gz"
chmod 0644 "$ROOT/Library/Printers/PPDs/Contents/Resources/Canon-LBP2900-Rust.ppd.gz"

install -m 0755 dist/bin/lbp2900-app "$ROOT/Applications/$APP/Contents/MacOS/lbp2900-app"
sed "s/@VERSION@/$VERSION/g" packaging/Info.plist > "$ROOT/Applications/$APP/Contents/Info.plist"
codesign --force --sign - "$ROOT/Applications/$APP"
install -m 0644 packaging/launchagent.plist "$ROOT/Library/LaunchAgents/$ID.app.plist"

chmod 0755 packaging/scripts/*
pkgbuild --root "$ROOT" --scripts packaging/scripts --identifier "$ID" --version "$VERSION" \
  --install-location / --ownership recommended dist/component.pkg
productbuild --package dist/component.pkg "dist/LBP2900-Driver-$VERSION.pkg"
rm dist/component.pkg
echo "built dist/LBP2900-Driver-$VERSION.pkg"
