#!/bin/sh
set -eu

cd "$(dirname "$0")"
APP="build/RichContentWebKitProbe.app"
BIN="$APP/Contents/MacOS/RichContentWebKitProbe"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp Info.plist "$APP/Contents/Info.plist"
/usr/bin/swiftc -O -framework AppKit -framework WebKit -framework PDFKit -framework CryptoKit -o "$BIN" main.swift
/usr/bin/codesign --force --sign - --entitlements Entitlements.plist --options runtime "$APP"
ENTITLEMENTS=$(/usr/bin/codesign -d --entitlements - "$APP" 2>&1)
printf '%s\n' "$ENTITLEMENTS" | /usr/bin/grep -q 'com.apple.security.app-sandbox'
printf '%s\n' "$ENTITLEMENTS" | /usr/bin/grep -q 'com.apple.security.network.client'
