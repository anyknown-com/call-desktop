#!/usr/bin/env bash
# Build a macOS .app bundle (ad-hoc signed) into dist/voice.app and optionally install it.
#   scripts/bundle-macos.sh            # → dist/voice.app
#   scripts/bundle-macos.sh --install  # also copies to /Applications (or ~/Applications)
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION=$(grep -m1 '^version' crates/voice-app/Cargo.toml | sed 's/.*"\(.*\)"/\1/')
[ -f models/campplus.onnx ] && [ -f models/silero_vad_v5.onnx ] || ./scripts/fetch-models.sh
cargo build --release -p voice-app -p voice-cli
APP=dist/voice.app
rm -rf "$APP"; mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/models"
cp target/release/voice-app "$APP/Contents/MacOS/voice-app"
cp target/release/voice "$APP/Contents/MacOS/voice"          # CLI ships alongside
cp models/*.onnx "$APP/Contents/Resources/models/"
cp -R models/campplus "$APP/Contents/Resources/models/"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>voice</string>
  <key>CFBundleDisplayName</key><string>voice</string>
  <key>CFBundleIdentifier</key><string>com.anyknown.voice</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleExecutable</key><string>voice-app</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>14.2</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>voice listens to you so the assistant can hear what you say.</string>
  <key>NSAudioCaptureUsageDescription</key><string>Needed to mute other apps' audio while the assistant speaks (a Core Audio process tap; nothing is recorded).</string>
</dict></plist>
PLIST
codesign --force --deep --sign - "$APP"
echo "built $APP (v$VERSION)"
if [ "${1:-}" = "--install" ]; then
  DEST=/Applications; [ -w "$DEST" ] || DEST="$HOME/Applications"; mkdir -p "$DEST"
  rm -rf "$DEST/voice.app"; cp -R "$APP" "$DEST/"
  echo "installed $DEST/voice.app"
fi
