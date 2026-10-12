#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

cargo build --locked --release
swift build --package-path macos --configuration release
swift_bin=$(swift build --package-path macos --configuration release --show-bin-path)
bundle="$root/target/macos/Sprowt.app"
mkdir -p "$bundle/Contents/MacOS"
cp "$swift_bin/SprowtDesktop" "$bundle/Contents/MacOS/Sprowt"
cp "$root/target/release/sprowt-harness" "$bundle/Contents/MacOS/sprowt-harness"
cp macos/Info.plist "$bundle/Contents/Info.plist"
codesign --force --deep --sign - "$bundle"
printf '%s\n' "$bundle"
