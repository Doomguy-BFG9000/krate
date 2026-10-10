#!/usr/bin/env bash
#
# Package and install the iOS player on a real, personally-owned iPhone,
# signed with a free personal team. No paid account needed; the signature
# lasts 7 days and then the app must be reinstalled.
#
# One-time setup (the human's part -- these steps need an Apple ID and a
# password, so no script can do them):
#
#   1. sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
#   2. Xcode > Settings > Accounts > add your Apple ID (a "Personal Team"
#      appears).
#   3. Make a throwaway iOS App project in Xcode with bundle identifier
#      tech.krate.player, team = your Personal Team, plug in the iPhone,
#      run it once on the device. That single run makes Xcode mint the
#      signing certificate, register the device, and write a provisioning
#      profile -- the two artifacts this script reuses.
#   4. On the iPhone: Settings > Privacy & Security > Developer Mode > on
#      (reboots), and trust the computer when asked.
#
# Then:
#   IDENTITY="Apple Development: you@example.com (TEAMID123)" \
#   PROFILE=/path/to/embedded.mobileprovision \
#   scripts/package-ios-device.sh
#
# A wrapper -- one app carried as native code (see crates/player-ios/build.rs)
# -- gets its own identity on the home screen:
#   BUNDLE_ID=tech.krate.cs NAME="Counter-Strike" LANDSCAPE=1 FILES=1
# LANDSCAPE=1 holds the app sideways (games); FILES=1 shows its folder in
# the Files app and Finder, so a person can bring the app's data.
#
# Find the identity name:  security find-identity -v -p codesigning
# Find the profile: ~/Library/Developer/Xcode/DerivedData/<throwaway>/
#   Build/Products/Debug-iphoneos/<app>.app/embedded.mobileprovision
# (or ~/Library/MobileDevice/Provisioning Profiles/, newest file)

set -euo pipefail
cd "$(dirname "$0")/.."

BIN=target/aarch64-apple-ios/release/krate-player-ios
OUT=target/ios-device/KratePlayer.app
BUNDLE_ID="${BUNDLE_ID:-tech.krate.player}"
NAME="${NAME:-Krate}"
: "${IDENTITY:?set IDENTITY to your Apple Development signing identity}"
: "${PROFILE:?set PROFILE to the .mobileprovision path}"
[ -f "$BIN" ] || { echo "error: build the device binary first:
  cargo build --release -p krate-player-ios --target aarch64-apple-ios" >&2; exit 1; }

rm -rf "$OUT" && mkdir -p "$OUT"
cp "$BIN" "$OUT/KratePlayer"
cp "$PROFILE" "$OUT/embedded.mobileprovision"
# The home-screen icon: the desktop app's 1024 px mark on a full-bleed
# square (iOS rounds the corners itself), compiled into an asset catalog --
# loose PNGs were upscaled from 60 pt and showed blurry and over-cropped.
# The mark the loading cover shows (the site's logo, transparent).
cp docs/landing/krate-logo.png "$OUT/KrateMark.png"
xcrun actool crates/player-ios/icons/Assets.xcassets --compile "$OUT" \
  --platform iphoneos --minimum-deployment-target 15.0 --app-icon AppIcon \
  --target-device iphone --output-partial-info-plist /tmp/krate-icon.plist >/dev/null
EXTRA=""
if [ "${LANDSCAPE:-0}" = 1 ]; then
  EXTRA="$EXTRA<key>UISupportedInterfaceOrientations</key><array><string>UIInterfaceOrientationLandscapeRight</string><string>UIInterfaceOrientationLandscapeLeft</string></array><key>UIRequiresFullScreen</key><true/><key>UIStatusBarHidden</key><true/><key>UIViewControllerBasedStatusBarAppearance</key><false/>"
else
  # Every way up but upside down: the app's own view controller picks
  # portrait for its screens and landscape for a wide window (a game).
  EXTRA="$EXTRA<key>UISupportedInterfaceOrientations</key><array><string>UIInterfaceOrientationPortrait</string><string>UIInterfaceOrientationLandscapeRight</string><string>UIInterfaceOrientationLandscapeLeft</string></array><key>UIRequiresFullScreen</key><true/>"
fi
if [ "${FILES:-0}" = 1 ]; then
  EXTRA="$EXTRA<key>UIFileSharingEnabled</key><true/>"
fi
# Krate itself owns .krate files: "Open in Krate" from Messages, Mail,
# AirDrop and Files. iOS copies an opened file into Documents/Inbox (not in
# place), which is what the player reads. Same type id as the Mac app.
if [ "$BUNDLE_ID" = tech.krate.player ]; then
  EXTRA="$EXTRA<key>CFBundleDocumentTypes</key><array><dict><key>CFBundleTypeName</key><string>Krate app</string><key>CFBundleTypeRole</key><string>Viewer</string><key>LSHandlerRank</key><string>Owner</string><key>LSItemContentTypes</key><array><string>dev.krate.bundle</string></array></dict></array>"
  EXTRA="$EXTRA<key>UTExportedTypeDeclarations</key><array><dict><key>UTTypeIdentifier</key><string>dev.krate.bundle</string><key>UTTypeDescription</key><string>Krate app</string><key>UTTypeConformsTo</key><array><string>public.data</string></array><key>UTTypeTagSpecification</key><dict><key>public.filename-extension</key><array><string>krate</string></array></dict></dict></array>"
fi
EXTRA="$EXTRA<key>LSSupportsOpeningDocumentsInPlace</key><false/>"
cat > "$OUT/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleName</key><string>$NAME</string>
  <key>CFBundleDisplayName</key><string>$NAME</string>
  <key>CFBundleExecutable</key><string>KratePlayer</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>CFBundleShortVersionString</key><string>0.1.9</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>MinimumOSVersion</key><string>15.0</string>
  <key>CADisableMinimumFrameDurationOnPhone</key><true/>
  <key>UIBackgroundModes</key><array><string>audio</string></array>
  <key>UIDeviceFamily</key><array><integer>1</integer></array>
  <key>UILaunchScreen</key><dict/>
  <key>NSCameraUsageDescription</key><string>An app you opened in Krate asked to use the camera, and you allowed it on Krate's sheet. It sees through it only while it is open.</string>
  <key>NSMicrophoneUsageDescription</key><string>An app you opened in Krate asked to use the microphone, and you allowed it on Krate's sheet. It hears only while it is open.</string>
  <key>NSBluetoothAlwaysUsageDescription</key><string>An app you opened in Krate asked to reach a Bluetooth device, and you allowed it on Krate's sheet. It reaches only the device you pick.</string>
  <key>NSLocationWhenInUseUsageDescription</key><string>An app you opened in Krate asked where you are, and you allowed it on Krate's sheet. It is told only while it is open.</string>
  $EXTRA
  <key>CFBundleIcons</key><dict>
    <key>CFBundlePrimaryIcon</key><dict>
      <key>CFBundleIconFiles</key><array><string>AppIcon60x60</string></array>
      <key>CFBundleIconName</key><string>AppIcon</string>
    </dict>
  </dict>
</dict></plist>
PLIST

# The entitlements must match what the provisioning profile grants; the
# minimal personal-team set is the application identifier and team id,
# extracted straight from the profile so they cannot disagree.
security cms -D -i "$PROFILE" > /tmp/krate-profile.plist
/usr/libexec/PlistBuddy -x -c 'Print :Entitlements' /tmp/krate-profile.plist \
  > /tmp/krate-entitlements.plist
# A team's wildcard profile grants "TEAMID.*"; the app is signed for one id.
TEAM=$(/usr/libexec/PlistBuddy -c 'Print :Entitlements:com.apple.developer.team-identifier' /tmp/krate-profile.plist)
/usr/libexec/PlistBuddy -c "Set :application-identifier $TEAM.$BUNDLE_ID" /tmp/krate-entitlements.plist
/usr/libexec/PlistBuddy -c 'Delete :keychain-access-groups' /tmp/krate-entitlements.plist 2>/dev/null || true
codesign --force --sign "$IDENTITY" \
  --entitlements /tmp/krate-entitlements.plist "$OUT"
echo "signed: $OUT"

# Install over USB (or trusted Wi-Fi). devicectl ships with Xcode 15+.
# The UUID column, not the model column: device names contain spaces, so
# field splitting lies. The identifier is the only 36-char hex-dash token.
# With several iPhones paired, guessing installed onto somebody else's
# phone once -- so multiple matches now require an explicit DEVICE=<uuid>.
FOUND=$(xcrun devicectl list devices 2>/dev/null \
  | grep 'available (paired)' | grep iPhone \
  | grep -oE '[0-9A-F]{8}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{12}')
COUNT=$(echo "$FOUND" | grep -c . || true)
if [ -z "${DEVICE:-}" ] && [ "$COUNT" -gt 1 ]; then
  echo "several iPhones are paired; pick one and rerun with DEVICE=<uuid>:" >&2
  xcrun devicectl list devices | grep 'available (paired)' | grep iPhone >&2
  exit 1
fi
DEVICE="${DEVICE:-$(echo "$FOUND" | head -1)}"
if [ -n "${DEVICE:-}" ]; then
  xcrun devicectl device install app --device "$DEVICE" "$OUT"
  echo "installed on $DEVICE -- the Krate icon is on your home screen"
else
  echo "no iPhone found; plug it in and rerun, or install manually:"
  echo "  xcrun devicectl device install app --device <id> $OUT"
fi
