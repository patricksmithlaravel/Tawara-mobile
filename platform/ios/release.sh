#!/usr/bin/env bash
# Tawara for iOS: the App Store upload, Tawara-X.Y.Z+N.ipa (Tawara-wallet's
# docs/RELEASE.md section 6.5; docs/DECISIONS.md D36 and D32 item 8). It builds
# crates/mobile's `tawara` binary for aarch64-apple-ios, makes Tawara.app from
# Info.plist.in as platform/ios/tawara.sh does, adds what the store needs, signs
# the bundle and zips its Payload. It never uploads, validates or contacts an
# account: it prints the upload command for the owner to run.
#
# Run it on the owner's Mac, from the commit that is tagged for the upload
# (docs/RELEASE.md sections 5.1 to 5.4). Keys, certificates, team identifiers and
# profiles are never written into a file the repository keeps: they come from the
# environment variables below, and the generated entitlements stay in the output
# folder.
#
#   TAWARA_EXPORT_COMPLIANCE  required in every mode: true or false, counsel's
#                             answer on encryption beyond the exempt kinds
#                             (docs/RELEASE.md section 1.2). It is written as
#                             ITSAppUsesNonExemptEncryption. There is no default.
#   TAWARA_IOS_DRY_RUN=1      build and check everything, then stop before signing.
#                             A dry run is never uploaded, so it accepts an SDK
#                             other than 26.x and a tree that differs from its
#                             commit, with a warning.
#   TAWARA_BUILD_NUMBER       required for a release: the committed counter. Raise
#                             it with `platform/build-number.sh bump N`, commit
#                             that, then set this to N. A dry run takes the
#                             development number.
#   TAWARA_IOS_ICON_DIR       required for a release: the folder holding
#                             Assets.xcassets, with an AppIcon set. It is the
#                             owner's icon (docs/RELEASE.md section 1.2). A dry run
#                             without it leaves the icon out, and says so.
#   TAWARA_TEAM_ID            signing: the 10-character Apple team identifier.
#   TAWARA_IOS_PROFILE        signing: the App Store provisioning profile.
#   TAWARA_IOS_SIGN_ID        signing: the codesign identity, the Apple Distribution
#                             certificate's name.
#   DEVELOPER_DIR             the Xcode to build with. When it is unset and
#                             /Applications/Xcode_26.6.app exists, that one is used.
#   TAWARA_IOS_OUT            where the bundle and the .ipa are written (default
#                             target/ios-release, which git ignores).
#
# When TAWARA_TEAM_ID, TAWARA_IOS_PROFILE or TAWARA_IOS_SIGN_ID is unset, the
# script stops before signing, names what is unset, makes no .ipa and prints no
# upload command. It exits 0 there, so the last line says whether an .ipa exists.
set -Eeuo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# The build number (docs/RELEASE.md section 5.2): bn_number gives the number that
# CFBundleVersion takes. See platform/build-number.sh.
# shellcheck source=../build-number.sh
. "$ROOT/platform/build-number.sh"

BIN=tawara
BUNDLE=com.patricksmithlaravel.tawara
# The minimum system the release is built for. MinimumOSVersion and the binary's
# minos both take it, as CI's device job sets it.
MINOS=16.0
export IPHONEOS_DEPLOYMENT_TARGET=$MINOS
OUT=${TAWARA_IOS_OUT:-$ROOT/target/ios-release}
case $OUT in
  /*) ;;
  *) OUT=$ROOT/$OUT ;;
esac

die() { echo "release.sh: error: $*" >&2; exit 1; }
warn() { echo "release.sh: warning: $*" >&2; }
note() { echo "release.sh: $*"; }

# package_version: tawara-mobile's version, from cargo metadata, as tawara.sh reads it.
package_version() {
  cargo metadata --locked --no-deps --format-version 1 \
    | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="tawara-mobile"))'
}

# target_dir: cargo's target directory for this workspace. CARGO_TARGET_DIR, or a
# build.target-dir setting, moves it away from target/, so the binary is read from
# where cargo writes it.
target_dir() {
  cargo metadata --locked --no-deps --format-version 1 \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])'
}

# locked_app_version: the version of tawara-app, the wallet crate, in Cargo.lock.
# docs/RELEASE.md section 5.1 requires it to equal tawara-mobile's.
locked_app_version() {
  awk -F'"' '/^name = "tawara-app"$/ { getline; if ($1 == "version = ") { print $2; exit } }' "$ROOT/Cargo.lock"
}

# vtool_field BINARY FIELD: a field of the binary's LC_BUILD_VERSION. The awk reads
# to the end of vtool's output, so that no pipe is closed early.
vtool_field() {
  xcrun vtool -show-build "$1" | awk -v f="$2" '$1 == f && !done { print $2; done = 1 }'
}

# xcode_dt_version 26.6: the form of an Xcode version that the DTXcode key takes, 2660.
xcode_dt_version() {
  local major minor patch
  IFS=. read -r major minor patch <<<"$1"
  echo $((major * 100 + minor * 10 + ${patch:-0}))
}

# write_entitlements FILE TEAM: the entitlements the signed bundle carries (D32
# item 3; docs/RELEASE.md section 6.5). There is no keychain-access-groups entry:
# the application identifier is the default access group, and the Keychain item
# that D45 keeps belongs to this app alone. Without an application identifier the
# Keychain add fails with -34018 (platform/ios/README.md).
write_entitlements() {
  cat >"$1" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>application-identifier</key>
	<string>$2.$BUNDLE</string>
	<key>com.apple.developer.team-identifier</key>
	<string>$2</string>
	<key>com.apple.developer.default-data-protection</key>
	<string>NSFileProtectionComplete</string>
	<key>get-task-allow</key>
	<false/>
</dict>
</plist>
PLIST
}

# check_profile PLIST TEAM: the decoded profile is an App Store profile for this app.
check_profile() {
  python3 - "$1" "$2.$BUNDLE" <<'PY'
import plistlib, sys
with open(sys.argv[1], "rb") as f:
    profile = plistlib.load(f)
entitlements = profile.get("Entitlements", {})
if entitlements.get("application-identifier") != sys.argv[2]:
    sys.exit("the profile is for %r, not %r" % (entitlements.get("application-identifier"), sys.argv[2]))
if entitlements.get("get-task-allow") is not False:
    sys.exit("the profile does not say that get-task-allow is false")
if "ProvisionedDevices" in profile:
    sys.exit("the profile lists devices, so it is not an App Store profile")
PY
}

# check_signed READ EXPECTED: what codesign reads back from the signed bundle is
# the entitlements file that the bundle was signed with.
check_signed() {
  python3 - "$1" "$2" <<'PY'
import plistlib, sys
with open(sys.argv[1], "rb") as f:
    read = plistlib.load(f)
with open(sys.argv[2], "rb") as f:
    want = plistlib.load(f)
if read != want:
    sys.exit("codesign reads back %r, not %r" % (read, want))
PY
}

# sign_bundle APP ENTITLEMENTS IDENTITY: signs APP, verifies it, and reads back the
# entitlements it carries (docs/RELEASE.md section 6.5).
sign_bundle() {
  codesign --force --timestamp --sign "$3" --entitlements "$2" "$1"
  codesign --verify --strict --verbose=2 "$1"
  codesign -d --entitlements :- "$1" >"$OUT/entitlements.signed.plist" 2>/dev/null \
    || die "codesign cannot read the entitlements back from $1"
  check_signed "$OUT/entitlements.signed.plist" "$2" \
    || die "the signed bundle does not carry the entitlements it was signed with"
}

# package_ipa PARENT VERSION BUILD: zips PARENT/Payload into Tawara-VERSION+BUILD.ipa
# with ditto, as docs/RELEASE.md section 6.5 and D36 give it, and checks the archive:
# Payload/Tawara.app is in it, and no AppleDouble ._ entry is. Prints the path.
package_ipa() {
  local ipa=$OUT/Tawara-$2+$3.ipa
  rm -f "$ipa"
  (cd "$1" && ditto -c -k --noextattr --norsrc --noqtn --keepParent Payload "$ipa")
  unzip -Z1 "$ipa" >"$OUT/ipa.list" || die "unzip cannot list $ipa"
  grep "^Payload/Tawara\.app/" "$OUT/ipa.list" >/dev/null || die "$ipa has no Payload/Tawara.app/ entries"
  if grep -E "(^|/)\._" "$OUT/ipa.list" >/dev/null; then
    die "$ipa has AppleDouble ._ entries, which App Store Connect refuses"
  fi
  echo "$ipa"
}

main() {
  cd "$ROOT"
  local dry=0 export_yes version app_version build sdk xcode_version xcode_build exe linked_sdk tdir
  case ${TAWARA_IOS_DRY_RUN:-0} in
    0) ;;
    1) dry=1 ;;
    *) die "TAWARA_IOS_DRY_RUN is '$TAWARA_IOS_DRY_RUN': use 1 for a dry run, or leave it unset" ;;
  esac
  if [ "$dry" = 1 ]; then
    note "dry run: builds and checks everything, and signs and zips nothing"
  fi

  case ${TAWARA_EXPORT_COMPLIANCE:-} in
    true) export_yes=YES ;;
    false) export_yes=NO ;;
    *) die "TAWARA_EXPORT_COMPLIANCE must be true or false, counsel's answer on encryption (docs/RELEASE.md section 1.2). There is no default, so nothing is built without it" ;;
  esac

  # The inputs a release needs, checked before any tool runs. A dry run only warns
  # about the ones it can do without.
  if [ "$dry" = 0 ]; then
    [ -n "${TAWARA_BUILD_NUMBER:-}" ] || die "TAWARA_BUILD_NUMBER is not set. A release takes the committed build number (docs/RELEASE.md section 5.2): run 'platform/build-number.sh bump N', commit it, then set TAWARA_BUILD_NUMBER=N"
    [ -n "${TAWARA_IOS_ICON_DIR:-}" ] || die "TAWARA_IOS_ICON_DIR is not set. A release needs the owner's app icon, a folder holding Assets.xcassets with an AppIcon set (docs/RELEASE.md section 1.2)"
    if [ -n "$(git -C "$ROOT" status --porcelain --untracked-files=no)" ]; then
      die "tracked files differ from the commit. A release is built from the commit it is tagged on (docs/RELEASE.md section 5.4)"
    fi
  elif [ -n "$(git -C "$ROOT" status --porcelain --untracked-files=no)" ]; then
    warn "tracked files differ from the commit: a release would refuse this tree"
  fi
  if [ -n "${TAWARA_IOS_ICON_DIR:-}" ]; then
    [ -d "$TAWARA_IOS_ICON_DIR/Assets.xcassets/AppIcon.appiconset" ] \
      || die "$TAWARA_IOS_ICON_DIR/Assets.xcassets/AppIcon.appiconset is missing"
  else
    warn "TAWARA_IOS_ICON_DIR is not set: this dry run leaves the app icon out"
  fi

  if [ -z "${DEVELOPER_DIR:-}" ] && [ -d /Applications/Xcode_26.6.app/Contents/Developer ]; then
    export DEVELOPER_DIR=/Applications/Xcode_26.6.app/Contents/Developer
  fi
  xcodebuild -version >/dev/null 2>&1 \
    || die "no usable Xcode (DEVELOPER_DIR is '${DEVELOPER_DIR:-}'; docs/RELEASE.md section 1.6)"
  xcode_version=$(xcodebuild -version | awk 'NR == 1 { print $2 }')
  xcode_build=$(xcodebuild -version | awk '$1 == "Build" { print $3 }')
  sdk=$(xcrun --sdk iphoneos --show-sdk-version) || die "no iPhoneOS SDK under DEVELOPER_DIR '${DEVELOPER_DIR:-}'"
  note "Xcode $xcode_version ($xcode_build), iPhoneOS SDK $sdk"
  case $sdk in
    26.*) ;;
    *)
      if [ "$dry" = 1 ]; then
        warn "the iPhoneOS SDK is $sdk, not 26.x: this dry run is not for upload"
      else
        die "the iPhoneOS SDK is $sdk, not 26.x. An App Store upload needs Xcode 26.x and the iOS 26 SDK (docs/RELEASE.md sections 1.6 and 6.5; DECISIONS D36). Set DEVELOPER_DIR to Xcode 26.6"
      fi
      ;;
  esac

  # The SDK and Xcode values of the DT keys (docs/RELEASE.md section 6.5). They are
  # read here, before anything is built, so that a value which cannot be read stops
  # the script instead of becoming an empty key. The platform build number is the
  # ProductBuildVersion of the platform's version.plist.
  local platform_path platform_version platform_build sdk_build dt_xcode
  platform_path=$(xcrun --sdk iphoneos --show-sdk-platform-path) || die "no iPhoneOS platform path"
  platform_version=$(xcrun --sdk iphoneos --show-sdk-platform-version) || die "no iPhoneOS platform version"
  platform_build=$(plutil -extract ProductBuildVersion raw -o - "$platform_path/version.plist") \
    || die "no ProductBuildVersion in $platform_path/version.plist"
  sdk_build=$(xcrun --sdk iphoneos --show-sdk-build-version) || die "no iPhoneOS SDK build version"
  dt_xcode=$(xcode_dt_version "$xcode_version") || die "cannot read the Xcode version '$xcode_version'"
  if [ -z "$platform_version" ] || [ -z "$platform_build" ] || [ -z "$sdk_build" ] \
    || [ -z "$dt_xcode" ] || [ -z "$xcode_build" ]; then
    die "an SDK or Xcode value for the DT keys is empty (platform $platform_version $platform_build; SDK build $sdk_build; Xcode $xcode_version $xcode_build)"
  fi

  build=$(bn_number) || die "TAWARA_BUILD_NUMBER does not match platform/build-number (see the message above)"
  version=$(package_version)
  app_version=$(locked_app_version)
  [ "$version" = "$app_version" ] \
    || die "tawara-mobile is $version and tawara-app in Cargo.lock is $app_version: a release needs them equal (docs/RELEASE.md section 5.1)"
  note "version $version, build $build"

  note "building $BIN for aarch64-apple-ios"
  # The binary is read from cargo's target directory, which CARGO_TARGET_DIR can move.
  # An earlier binary is removed first, so that a build which leaves none cannot pass
  # the checks below with a stale one.
  tdir=$(target_dir) || die "cargo metadata gave no target directory"
  exe=$tdir/aarch64-apple-ios/release/$BIN
  rm -f "$exe"
  cargo build --locked --release -p tawara-mobile --bin "$BIN" --target aarch64-apple-ios
  [ -f "$exe" ] || die "$exe was not built"
  # LC_BUILD_VERSION: IOS is a phone build, and the SDK is the one it was linked with.
  [ "$(vtool_field "$exe" platform)" = IOS ] || die "$exe is not an iOS device binary"
  [ "$(vtool_field "$exe" minos)" = "$MINOS" ] || die "$exe has minos $(vtool_field "$exe" minos), not $MINOS"
  linked_sdk=$(vtool_field "$exe" sdk)
  case $linked_sdk in
    26.*) ;;
    *)
      if [ "$dry" = 1 ]; then
        warn "$BIN was linked with the iOS $linked_sdk SDK, not 26.x: this dry run is not for upload"
      else
        die "$BIN was linked with the iOS $linked_sdk SDK, not 26.x: the release was built with the wrong Xcode (docs/RELEASE.md section 6.5; DECISIONS D32 item 8)"
      fi
      ;;
  esac
  note "$BIN links with the iOS $linked_sdk SDK, minimum $MINOS"

  local app=$OUT/Tawara.app plist=$OUT/Tawara.app/Info.plist
  rm -rf "$app" "$OUT/ipa"
  mkdir -p "$app"
  cp "$exe" "$app/$BIN"
  sed -e 's/@PLATFORM@/iPhoneOS/' -e "s/@MINOS@/$MINOS/" -e "s/@VERSION@/$version/" -e "s/@BUILD@/$build/" \
    "$HERE/Info.plist.in" >"$plist"
  # The DT keys are those of the SDK and Xcode that built the binary (docs/RELEASE.md section 6.5).
  plutil -replace DTPlatformName -string iphoneos "$plist"
  plutil -replace DTPlatformVersion -string "$platform_version" "$plist"
  plutil -replace DTPlatformBuild -string "$platform_build" "$plist"
  plutil -replace DTSDKName -string "iphoneos$sdk" "$plist"
  plutil -replace DTSDKBuild -string "$sdk_build" "$plist"
  plutil -replace DTXcode -string "$dt_xcode" "$plist"
  plutil -replace DTXcodeBuild -string "$xcode_build" "$plist"
  plutil -replace ITSAppUsesNonExemptEncryption -bool "$export_yes" "$plist"

  if [ -n "${TAWARA_IOS_ICON_DIR:-}" ]; then
    local partial=$OUT/icon-partial.plist
    xcrun actool "$TAWARA_IOS_ICON_DIR/Assets.xcassets" --compile "$app" \
      --platform iphoneos --minimum-deployment-target "$MINOS" --target-device iphone \
      --app-icon AppIcon --output-partial-info-plist "$partial" \
      --errors --warnings --notices >"$OUT/actool.txt" 2>&1 \
      || { cat "$OUT/actool.txt" >&2; die "actool could not compile the icon"; }
    python3 - "$plist" "$partial" <<'PY'
import plistlib, sys
with open(sys.argv[1], "rb") as f:
    info = plistlib.load(f)
with open(sys.argv[2], "rb") as f:
    info.update(plistlib.load(f))
with open(sys.argv[1], "wb") as f:
    plistlib.dump(info, f)
PY
    [ -f "$app/Assets.car" ] || die "actool made no Assets.car"
    note "app icon compiled into Assets.car"
  fi

  # The privacy manifest and the notices, which docs/RELEASE.md section 4 puts at the
  # root of the iOS bundle.
  cp "$HERE/PrivacyInfo.xcprivacy" "$app/PrivacyInfo.xcprivacy"
  cp "$ROOT/THIRD-PARTY-NOTICES.txt" "$ROOT/LICENSE.md" "$app/"

  plutil -lint "$plist" "$app/PrivacyInfo.xcprivacy" >/dev/null || die "a plist does not lint"
  plutil -extract UIApplicationSceneManifest.UISceneConfigurations xml1 -o /dev/null "$plist" \
    || die "Info.plist has no scene manifest (DECISIONS D32 item 8)"
  [ "$(plutil -extract CFBundleVersion raw -o - "$plist")" = "$build" ] || die "CFBundleVersion is not $build"
  [ "$(plutil -extract CFBundleShortVersionString raw -o - "$plist")" = "$version" ] \
    || die "CFBundleShortVersionString is not $version"
  [ "$(plutil -extract NSPrivacyTracking raw -o - "$app/PrivacyInfo.xcprivacy")" = false ] \
    || die "PrivacyInfo.xcprivacy: NSPrivacyTracking is not false"
  note "Info.plist and PrivacyInfo.xcprivacy lint; the scene manifest is there"

  if [ "$dry" = 1 ]; then
    note "dry run: stopped before signing. Nothing was signed, zipped or prepared for upload."
    note "the unsigned bundle is $app"
    return 0
  fi

  local unset_list="" team
  [ -n "${TAWARA_TEAM_ID:-}" ] || unset_list="$unset_list TAWARA_TEAM_ID"
  [ -n "${TAWARA_IOS_PROFILE:-}" ] || unset_list="$unset_list TAWARA_IOS_PROFILE"
  [ -n "${TAWARA_IOS_SIGN_ID:-}" ] || unset_list="$unset_list TAWARA_IOS_SIGN_ID"
  if [ -n "$unset_list" ]; then
    note "SKIPPED signing, because these are unset:$unset_list. Set them to sign (docs/RELEASE.md section 6.5)."
    note "no .ipa was made. The unsigned bundle is $app"
    return 0
  fi
  team=$TAWARA_TEAM_ID
  [ "${#team}" -eq 10 ] \
    && [ "$(printf '%s' "$team" | tr -cd 'ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789' | wc -c | tr -d ' ')" -eq 10 ] \
    || die "TAWARA_TEAM_ID must be the 10-character Apple team identifier"
  [ -f "$TAWARA_IOS_PROFILE" ] || die "TAWARA_IOS_PROFILE is not a file"
  security cms -D -i "$TAWARA_IOS_PROFILE" >"$OUT/profile.plist" \
    || die "TAWARA_IOS_PROFILE is not a provisioning profile"
  check_profile "$OUT/profile.plist" "$team" \
    || die "TAWARA_IOS_PROFILE is not an App Store profile for this app"
  write_entitlements "$OUT/entitlements.plist" "$team"
  plutil -lint "$OUT/entitlements.plist" >/dev/null || die "the entitlements do not lint"

  cp "$TAWARA_IOS_PROFILE" "$app/embedded.mobileprovision"
  local payload=$OUT/ipa/Payload
  mkdir -p "$payload"
  mv "$app" "$payload/Tawara.app"
  app=$payload/Tawara.app
  sign_bundle "$app" "$OUT/entitlements.plist" "$TAWARA_IOS_SIGN_ID"
  note "signed; the entitlements read back as signed"

  local ipa
  ipa=$(package_ipa "$OUT/ipa" "$version" "$build")
  note "made $ipa"
  shasum -a 256 "$ipa"
  cat <<TXT
Not run by this script. For the owner, validate the package and then upload it with
an App Store Connect API key (docs/RELEASE.md section 6.5):
  xcrun altool --validate-app -f "$ipa" -t ios --apiKey <key id> --apiIssuer <issuer id>
  xcrun altool --upload-app -f "$ipa" -t ios --apiKey <key id> --apiIssuer <issuer id>
or Transporter, with the same file.
TXT
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  main "$@"
fi
