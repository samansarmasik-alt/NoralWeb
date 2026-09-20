#!/bin/sh
# NoralWeb GUI'yi macOS .app paketine sarar, ad-hoc imzalar, tar.gz yapar.
# Kullanim: pack-macos.sh <arm64|x64> <derlenmis-binary-yolu>
# Ornek: ./pack-macos.sh arm64 target/release/noral-web
set -e
ARCH="$1"
BIN="$2"
APP="NoralWeb.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/NoralWeb"
cat > "$APP/Contents/Info.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>NoralWeb</string>
  <key>CFBundleIdentifier</key><string>com.noralweb.app</string>
  <key>CFBundleVersion</key><string>0.37.0</string>
  <key>CFBundleShortVersionString</key><string>0.37.0</string>
  <key>CFBundleExecutable</key><string>NoralWeb</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF
codesign --force --deep -s - "$APP"
tar -czf "NoralWeb-macos-$ARCH.tar.gz" "$APP"
rm -rf "$APP"
echo "hazir: NoralWeb-macos-$ARCH.tar.gz"
