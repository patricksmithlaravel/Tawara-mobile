#!/usr/bin/env bash
# Tawara for iOS on a macOS runner, with plain commands only
# (docs/DECISIONS.md D13, D32):
#
#   platform/ios/tawara.sh device      # build and link for aarch64-apple-ios;
#                                      # installing on a phone needs signing
#   platform/ios/tawara.sh simulator   # build, bundle, sign ad hoc, boot a
#                                      # simulator, install, run the checks
#
# There is no Xcode project: the bundle is crates/mobile's `tawara` binary
# and an Info.plist made from Info.plist.in, as phase 1's spike was
# (docs/spikes/P1-REPORT.md). Signing for a device and the store
# (`com.apple.developer.default-data-protection`) is phase 5's.
#
# The application's stderr goes to the file `simctl launch --stderr` names:
# `TAWARA ...` lines, never a secret. This script prints `CHECK PASS|FAIL
# <name>: <detail>` lines, a table in the step summary and the screenshots
# as base64 JPEGs between markers, and fails if any check failed.
set -Eeuo pipefail

BIN=tawara
BUNDLE=com.patricksmithlaravel.tawara
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
OUT=${RUNNER_TEMP:-/tmp}/tawara-ios
export IPHONEOS_DEPLOYMENT_TARGET=${IPHONEOS_DEPLOYMENT_TARGET:-16.0}
mkdir -p "$OUT/shots"
: >"$OUT/results.tsv"

group() { echo "::group::$*"; }
endgroup() { echo "::endgroup::"; }
die() { echo "::error::$*"; exit 1; }
record() { # record NAME PASS|FAIL DETAIL
  printf '%s\t%s\t%s\n' "$1" "$2" "${3:-}" >>"$OUT/results.tsv"
  echo "CHECK $2 $1: ${3:-}"
}
check() { # check NAME DETAIL COMMAND...: PASS when the command succeeds
  local name=$1 detail=$2
  shift 2
  if "$@"; then record "$name" PASS "$detail"; else record "$name" FAIL "$detail"; fi
}
platform_of() { otool -l "$1" | awk '/LC_BUILD_VERSION/{f=1} f&&/platform/{print $2; exit}'; }
version() {
  cargo metadata --no-deps --format-version 1 \
    | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="tawara-mobile"))'
}

# -------------------------------------------------------------------- device
device() {
  xcodebuild -version
  cd "$ROOT"
  group "cargo build --target aarch64-apple-ios"
  cargo build --locked --release -p tawara-mobile --bin "$BIN" --target aarch64-apple-ios
  endgroup
  local exe=target/aarch64-apple-ios/release/$BIN
  file "$exe"
  # LC_BUILD_VERSION platform 2 is iOS on a device.
  check device.build "$(file -b "$exe")" test "$(platform_of "$exe")" = 2
  summary
  ! grep -q $'\tFAIL\t' "$OUT/results.tsv"
}

# ----------------------------------------------------------------- simulator
wait_after() { # wait_after FILE LINE ERE SECONDS: the first match after line LINE
  local t=0 line
  while :; do
    line=$(tail -n +"$(($2 + 1))" "$1" 2>/dev/null | grep -E -m1 "$3" || true)
    [ -n "$line" ] && { echo "$line"; return 0; }
    [ "$t" -ge "$4" ] && return 1
    sleep 1
    t=$((t + 1))
  done
}
shot() { xcrun simctl io "$UDID" screenshot --type=png "$OUT/shots/$1.png" >/dev/null 2>&1 || true; }

simulator() {
  xcodebuild -version
  cd "$ROOT"
  group "cargo build --target aarch64-apple-ios-sim"
  cargo build --locked --release -p tawara-mobile --bin "$BIN" --target aarch64-apple-ios-sim
  endgroup
  local exe=target/aarch64-apple-ios-sim/release/$BIN
  # LC_BUILD_VERSION platform 7 is the iOS simulator.
  [ "$(platform_of "$exe")" = 7 ] || die "$exe: platform $(platform_of "$exe"), expected 7"

  group "bundle and sign"
  local app=$OUT/Tawara.app
  rm -rf "$app"
  mkdir -p "$app"
  cp "$exe" "$app/$BIN"
  sed -e 's/@PLATFORM@/iPhoneSimulator/' -e "s/@MINOS@/$IPHONEOS_DEPLOYMENT_TARGET/" \
    -e "s/@VERSION@/$(version)/" platform/ios/Info.plist.in >"$app/Info.plist"
  plutil -lint "$app/Info.plist"
  # Without the scene manifest, an app linked with the iOS 27 SDK is
  # stopped by UIKit at launch (D32 item 8); older SDKs still run it, so the
  # simulator alone would not catch its loss.
  check bundle.scene_manifest "UIApplicationSceneManifest in Info.plist" \
    plutil -extract UIApplicationSceneManifest.UISceneConfigurations xml1 -o /dev/null "$app/Info.plist"
  codesign --force --sign - --timestamp=none "$app"
  codesign --verify --verbose=2 "$app"
  endgroup

  group "simulator"
  local runtime devtype
  read -r runtime devtype < <(python3 - <<'PY'
import json, subprocess
rts = json.loads(subprocess.check_output(["xcrun", "simctl", "list", "-j", "runtimes", "available"]))["runtimes"]
ios = [r for r in rts if r.get("platform") == "iOS"]
rt = max(ios, key=lambda r: tuple(int(x) for x in r["version"].split(".")))
phones = [d for d in rt.get("supportedDeviceTypes", []) if d.get("productFamily") == "iPhone" or d["name"].startswith("iPhone")]
named = [d for d in phones if d["name"] == "iPhone 17"]
print(rt["identifier"], (named or phones)[-1]["identifier"])
PY
  )
  echo "runtime $runtime; device type $devtype"
  UDID=$(xcrun simctl create tawara "$devtype" "$runtime")
  xcrun simctl boot "$UDID"
  xcrun simctl bootstatus "$UDID" -b
  xcrun simctl install "$UDID" "$app"
  endgroup
  trap 'evidence; xcrun simctl shutdown "$UDID" >/dev/null 2>&1 || true' EXIT

  local err=$OUT/app.err out pid pid2 line container
  group "start"
  out=$(SIMCTL_CHILD_RUST_BACKTRACE=1 xcrun simctl launch --stdout="$OUT/app.out" \
    --stderr="$err" "$UDID" "$BUNDLE")
  pid=$(awk '{print $NF}' <<<"$out")
  line=$(wait_after "$err" 0 '^TAWARA start: ' 60 || true)
  check start "${line:-no start line}" grep -q 'Library/Application Support/Tawara$' <<<"$line"
  line=$(wait_after "$err" 0 '^TAWARA store: ' 5 || true)
  check store.protected "${line:-no store line}" grep -q 'protected=true' <<<"$line"
  check store.excluded "${line:-no store line}" grep -q 'excluded_from_backup=true' <<<"$line"
  sleep 8
  shot 01-start
  endgroup

  group "the directory as the system holds it"
  # NSURLIsExcludedFromBackupKey is kept as this extended attribute.
  container=$(xcrun simctl get_app_container "$UDID" "$BUNDLE" data)
  local dir="$container/Library/Application Support/Tawara"
  ls -la "$dir" || true
  check backup.xattr "$(xattr -l "$dir" 2>&1 | head -2 | tr '\n' ' ')" \
    xattr -p com.apple.metadata:com_apple_backup_excludeItem "$dir"
  check dir.mode "$(stat -f '%Sp' "$dir")" test "$(stat -f '%Lp' "$dir")" = 700
  endgroup

  group "to the background and back"
  local front
  front=$(wc -l <"$err")
  xcrun simctl launch "$UDID" com.apple.Preferences >/dev/null
  line=$(wait_after "$err" "$front" '^TAWARA lifecycle: inactive; covered$' 20 || true)
  check background.covered "${line:-no cover line}" test -n "$line"
  # The scene life cycle (D32 item 8): the iced fork puts winit's window in
  # the scene the manifest declares, or nothing is shown. Read when the
  # cover goes on, well after launch, whichever order the scene and the
  # window came in.
  line=$(wait_after "$err" "$front" '^TAWARA scene: key window in scene=' 5 || true)
  check scene.window "${line:-no scene line}" grep -q 'scene=true$' <<<"$line"
  line=$(wait_after "$err" "$front" '^TAWARA lifecycle: background; locking$' 20 || true)
  check background.locks "${line:-no lock line}" test -n "$line"
  sleep 3
  shot 02-settings-in-front
  front=$(wc -l <"$err")
  out=$(xcrun simctl launch "$UDID" "$BUNDLE")
  pid2=$(awk '{print $NF}' <<<"$out")
  line=$(wait_after "$err" "$front" '^TAWARA lifecycle: active; uncovered$' 20 || true)
  check return.uncovered "${line:-no uncover line}" test -n "$line"
  check return.same_process "pid $pid, then $pid2" test "$pid" = "$pid2"
  sleep 5
  shot 03-returned
  endgroup

  check no_panic "no panic on stderr" bash -c "! grep -q 'panicked' '$err'"
}

summary() {
  {
    echo "### iOS checks"
    echo
    echo "| check | result | detail |"
    echo "|---|---|---|"
    while IFS=$'\t' read -r name result detail; do
      echo "| $name | $result | ${detail//|/\\|} |"
    done <"$OUT/results.tsv"
  } >>"${GITHUB_STEP_SUMMARY:-/dev/null}"
}

evidence() {
  # Screenshots first and the results last: a job log is read from its end.
  for f in "$OUT"/shots/*.png; do
    [ -f "$f" ] || continue
    local j=${f%.png}.jpg
    sips -Z 700 -s format jpeg "$f" --out "$j" >/dev/null
    echo "-----BEGIN SHOT $(basename "$j" .jpg)-----"
    base64 -i "$j" | fold -w 76
    echo "-----END SHOT $(basename "$j" .jpg)-----"
  done
  group "the application's stderr"
  cat "$OUT/app.err" 2>/dev/null || true
  endgroup
  summary
  echo "===== results ====="
  awk -F'\t' '{printf "CHECK %s %s: %s\n", $2, $1, $3}' "$OUT/results.tsv"
  if grep -q $'\tFAIL\t' "$OUT/results.tsv"; then
    echo "::error::the iOS checks have failures"
    exit 1
  fi
}

case ${1:-} in
  device) device ;;
  simulator) simulator ;;
  *) die "usage: $0 device|simulator" ;;
esac
