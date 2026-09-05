#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 4 ]]; then
  echo "usage: $0 <unsigned-apk> <unsigned-aab> <signed-apk> <signed-aab>" >&2
  exit 2
fi

unsigned_apk="$1"
unsigned_aab="$2"
signed_apk="$3"
signed_aab="$4"

for artifact in "$unsigned_apk" "$unsigned_aab"; do
  if [[ ! -f "$artifact" ]]; then
    echo "Android release artifact is missing: $artifact" >&2
    exit 1
  fi
done

if [[ "$signed_apk" == "$unsigned_apk" || "$signed_aab" == "$unsigned_aab" ]]; then
  echo "signed output paths must differ from unsigned input paths" >&2
  exit 1
fi

mkdir -p "$(dirname "$signed_apk")" "$(dirname "$signed_aab")"

find_android_tool() {
  local name="$1"
  if command -v "$name" >/dev/null 2>&1; then
    command -v "$name"
    return 0
  fi

  local sdk_root="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
  if [[ -z "$sdk_root" ]]; then
    return 1
  fi

  local preferred_version="${ANDROID_BUILD_TOOLS_VERSION:-35.0.0}"
  local preferred_path="$sdk_root/build-tools/$preferred_version/$name"
  if [[ -x "$preferred_path" ]]; then
    printf '%s\n' "$preferred_path"
    return 0
  fi

  local latest_path
  latest_path="$(find "$sdk_root/build-tools" -mindepth 1 -maxdepth 1 -type d -print 2>/dev/null | sort -V | tail -n 1)"
  if [[ -n "$latest_path" && -x "$latest_path/$name" ]]; then
    printf '%s\n' "$latest_path/$name"
    return 0
  fi
  return 1
}

apksigner="$(find_android_tool apksigner)" || {
  echo "apksigner is required from the Android build-tools" >&2
  exit 1
}
zipalign="$(find_android_tool zipalign)" || {
  echo "zipalign is required from the Android build-tools" >&2
  exit 1
}
command -v keytool >/dev/null 2>&1 || {
  echo "keytool is required to prepare Android signing material" >&2
  exit 1
}
command -v jarsigner >/dev/null 2>&1 || {
  echo "jarsigner is required to sign the Android App Bundle" >&2
  exit 1
}

signing_parent="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
mkdir -p "$signing_parent"
signing_root="$(mktemp -d "$signing_parent/vibex-android-signing.XXXXXX")"
keystore="$signing_root/release.keystore"
store_password_file="$signing_root/store-password"
key_password_file="$signing_root/key-password"

key_alias="${VIBEX_ANDROID_KEY_ALIAS:-vibex}"
keystore_base64="${VIBEX_ANDROID_KEYSTORE_BASE64:-}"
release_channel="${VIBEX_RELEASE_CHANNEL:-rc}"

if [[ -n "$keystore_base64" ]]; then
  : "${VIBEX_ANDROID_KEYSTORE_PASSWORD:?VIBEX_ANDROID_KEYSTORE_PASSWORD is required with VIBEX_ANDROID_KEYSTORE_BASE64}"
  printf '%s' "$keystore_base64" | base64 --decode > "$keystore"
  store_password="$VIBEX_ANDROID_KEYSTORE_PASSWORD"
  key_password="${VIBEX_ANDROID_KEY_PASSWORD:-$store_password}"
else
  if [[ "$release_channel" == "stable" ]]; then
    echo "VIBEX_ANDROID_KEYSTORE_BASE64 is required for stable Android releases" >&2
    exit 1
  fi
  key_alias="vibex-ci"
  store_password="vibex-ci-test-password"
  key_password="$store_password"
  keytool -genkeypair -noprompt \
    -keystore "$keystore" \
    -storetype PKCS12 \
    -storepass "$store_password" \
    -keypass "$key_password" \
    -alias "$key_alias" \
    -keyalg RSA \
    -keysize 2048 \
    -validity 10000 \
    -dname "CN=Vibex CI, OU=Vibex, O=Vibex, C=US" >/dev/null
fi

printf '%s' "$store_password" > "$store_password_file"
printf '%s' "$key_password" > "$key_password_file"
chmod 600 "$keystore" "$store_password_file" "$key_password_file"

aligned_apk="$signing_root/aligned.apk"
"$zipalign" -f -p 4 "$unsigned_apk" "$aligned_apk"
"$apksigner" sign \
  --ks "$keystore" \
  --ks-key-alias "$key_alias" \
  --ks-pass "file:$store_password_file" \
  --key-pass "file:$key_password_file" \
  --out "$signed_apk" \
  "$aligned_apk"
"$zipalign" -c -P 4 -v 4 "$signed_apk" >/dev/null
"$apksigner" verify --verbose "$signed_apk"

jarsigner \
  -keystore "$keystore" \
  -storetype PKCS12 \
  -storepass "$store_password" \
  -keypass "$key_password" \
  -sigalg SHA256withRSA \
  -digestalg SHA-256 \
  -signedjar "$signed_aab" \
  "$unsigned_aab" \
  "$key_alias" >/dev/null
jarsigner -verify "$signed_aab" >/dev/null

printf 'Signed Android APK: %s\nSigned Android AAB: %s\n' "$signed_apk" "$signed_aab"
