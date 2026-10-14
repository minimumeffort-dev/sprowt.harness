#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

cargo build --locked --release
swift build --package-path macos --configuration release
swift_bin=$(swift build --package-path macos --configuration release --show-bin-path)
bundle="$root/target/macos/Sprowt.app"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp "$swift_bin/SprowtDesktop" "$bundle/Contents/MacOS/Sprowt"
cp "$root/target/release/sprowt-harness" "$bundle/Contents/MacOS/sprowt-harness"
cp macos/Info.plist "$bundle/Contents/Info.plist"
iconset="$root/target/macos/Sprowt.iconset"
swift macos/Tools/GenerateAppIcon.swift "$iconset"
iconutil --convert icns --output "$bundle/Contents/Resources/Sprowt.icns" "$iconset"
if [ -d "$bundle/Contents/Resources/SprowtDesktop_SprowtDesktop.bundle" ]; then
    rm -r "$bundle/Contents/Resources/SprowtDesktop_SprowtDesktop.bundle"
fi
codesign --force --deep --sign - "$bundle"
printf '%s\n' "$bundle"
