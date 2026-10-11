#!/usr/bin/env bash
#
# Package the built Krate player .so into an installable APK.
#
# Packaging without Gradle: aapt2 links the manifest against android.jar,
# javac and d8 turn the player's one small activity (KrateActivity, the
# NativeActivity that loads libkrate_player.so) into classes.dex, the .so
# goes in as lib/arm64-v8a/, then zipalign and apksigner.
#
# d8 comes from build-tools 35 or newer: the 34 one fails with an internal
# error on classes javac 21 writes.
#
# The debug keystore is generated on first use and kept in target/ -- it
# signs dev builds only. A release keystore is a deliberate, separate step
# that does not live in this script.
#
# Run scripts/build-android.sh first.

set -euo pipefail

cd "$(dirname "$0")/.."

SDK="${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}"
BT="$SDK/build-tools/34.0.0"
D8_BT="${D8_BUILD_TOOLS:-$(ls -d "$SDK"/build-tools/3[5-9].* 2>/dev/null | sort -V | tail -1)}"
JAVA_SRC=crates/player-android/android/java
SO=target/aarch64-linux-android/release/libkrate_player.so
OUT=target/android
KEYSTORE="$OUT/debug.keystore"

if [ ! -f "$SO" ]; then
  echo "error: $SO not built -- run scripts/build-android.sh first" >&2
  exit 1
fi
if [ -z "${JAVA_HOME:-}" ] && command -v brew >/dev/null; then
  export JAVA_HOME="$(brew --prefix openjdk@21)/libexec/openjdk.jdk/Contents/Home"
fi
export PATH="$JAVA_HOME/bin:$PATH"
if [ -z "$D8_BT" ] || [ ! -x "$D8_BT/d8" ]; then
  echo "error: no d8 from build-tools 35 or newer -- sdkmanager \"build-tools;35.0.0\"" >&2
  exit 1
fi

rm -rf "$OUT/stage"
mkdir -p "$OUT/stage/lib/arm64-v8a"
cp "$SO" "$OUT/stage/lib/arm64-v8a/"

"$BT/aapt2" link -o "$OUT/stage/base.apk" \
  --manifest crates/player-android/android/AndroidManifest.xml \
  -I "$SDK/platforms/android-34/android.jar"

mkdir -p "$OUT/stage/classes"
javac -source 8 -target 8 -Xlint:-options \
  -bootclasspath "$SDK/platforms/android-34/android.jar" \
  -d "$OUT/stage/classes" $(find "$JAVA_SRC" -name '*.java')
"$D8_BT/d8" --min-api 26 --lib "$SDK/platforms/android-34/android.jar" \
  --output "$OUT/stage" $(find "$OUT/stage/classes" -name '*.class')

(cd "$OUT/stage" && zip -q base.apk classes.dex lib/arm64-v8a/libkrate_player.so)
"$BT/zipalign" -f -p 4 "$OUT/stage/base.apk" "$OUT/stage/aligned.apk"

if [ ! -f "$KEYSTORE" ]; then
  keytool -genkeypair -keystore "$KEYSTORE" -alias debug -keyalg RSA \
    -keysize 2048 -validity 10000 -storepass krate-debug \
    -dname "CN=Krate Debug" 2>/dev/null
fi

"$BT/apksigner" sign --ks "$KEYSTORE" --ks-pass pass:krate-debug \
  --out "$OUT/krate-player.apk" "$OUT/stage/aligned.apk"

echo "packaged: $OUT/krate-player.apk"
