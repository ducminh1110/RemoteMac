#!/usr/bin/env bash
# Builds Taro.app, Taro for Mac's Enterprise edition, from a built agent (the taro binary,
# universal in releases): the same program, which opens as an app (setup window, menu bar item;
# agent/macos/EnterpriseApp.swift) when started from the bundle with no arguments.
#   scripts/build-enterprise-app-macos.sh BINARY [PROVIDER_DIR] [OUT_DIR]
# PROVIDER_DIR (optional): a provider/ folder (provider.json, logo, icon), copied into
# Contents/Resources/provider. OUT_DIR: where Taro.app is made (dist). Signed ad hoc.
set -euo pipefail
cd "$(dirname "$0")/.."
BIN="${1:?usage: $0 BINARY [PROVIDER_DIR] [OUT_DIR]}"
PROVIDER="${2:-}"
OUT="${3:-dist}"
[[ -f "$BIN" ]] || { echo "no binary at $BIN" >&2; exit 2; }
[[ -z "$PROVIDER" || -f "$PROVIDER/provider.json" ]] || { echo "$PROVIDER has no provider.json" >&2; exit 2; }
VERSION="$(sed -n 's/^let appVersion = "\(.*\)"$/\1/p' agent/macos/Session.swift)"
[[ -n "$VERSION" ]] || { echo "no appVersion in agent/macos/Session.swift" >&2; exit 2; }

APP="$OUT/Taro.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
install -m 0755 "$BIN" "$APP/Contents/MacOS/taro"

# the icon: the provider's (icon.png) when there is one, else Taro's
ICON_PNG=docs/images/taro-icon.png
if [[ -n "$PROVIDER" && -f "$PROVIDER/icon.png" ]]; then ICON_PNG="$PROVIDER/icon.png"; fi
ICON_KEY=""
if [[ -f "$ICON_PNG" ]] && command -v iconutil >/dev/null && command -v sips >/dev/null; then
  SET="$(mktemp -d)/AppIcon.iconset"; mkdir -p "$SET"
  for s in 16 32 128 256 512; do
    sips -z $s $s "$ICON_PNG" --out "$SET/icon_${s}x${s}.png" >/dev/null
    d=$((s * 2))
    if [[ $d -le 512 ]]; then sips -z $d $d "$ICON_PNG" --out "$SET/icon_${s}x${s}@2x.png" >/dev/null; fi
  done
  if iconutil -c icns "$SET" -o "$APP/Contents/Resources/AppIcon.icns"; then
    ICON_KEY="<key>CFBundleIconFile</key><string>AppIcon</string>"
  else
    echo "warning: no icon (iconutil failed)" >&2
  fi
  rm -rf "$(dirname "$SET")"
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>com.taro.enterprise.agent</string>
  <key>CFBundleExecutable</key><string>taro</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleName</key><string>Taro</string>
  <key>CFBundleDisplayName</key><string>Taro</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSScreenCaptureUsageDescription</key><string>Taro sends this Mac's screen to the person you let connect.</string>
  <key>NSAppTransportSecurity</key><dict><key>NSAllowsLocalNetworking</key><true/></dict>
  $ICON_KEY
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist" >/dev/null 2>&1 || { plutil -lint "$APP/Contents/Info.plist"; exit 3; }

if [[ -n "$PROVIDER" ]]; then
  cp -R "$PROVIDER" "$APP/Contents/Resources/provider"
fi

codesign --force --deep -s - "$APP"
echo "built $APP (Taro $VERSION${PROVIDER:+, provider from $PROVIDER})"
