#!/usr/bin/env bash
# Tawara for Android, with plain commands only (docs/DECISIONS.md D13, D32):
#
#   platform/android/tawara.sh build      # libtawara_mobile.so, arm64-v8a and x86_64
#   platform/android/tawara.sh apk        # the APK, signed with a throwaway debug key
#   platform/android/tawara.sh emulator   # boot an emulator, install, run the checks
#
# No Java, Kotlin or Gradle: the activity is android.app.NativeActivity, the
# APK is linked with aapt2 and zipped, aligned and signed by hand, as phase
# 1's spike was (Tawara-wallet's docs/spikes/P1-REPORT.md). A release key is
# phase 5's.
#
# The application prints `TAWARA ...` lines on stderr, which android-activity
# sends to logcat under the tag RustStdoutStderr. This script prints
# `CHECK PASS|FAIL <name>: <detail>` lines and a table in the step summary,
# and fails if any check failed. After the first start, after the asset-path
# check and at the end of the run, it also prints the system's lines about the
# application's activity and process (`trace`), so that a start-up which ends
# says why in the job log.
set -Eeuo pipefail

PKG=com.patricksmithlaravel.tawara
ACTIVITY=android.app.NativeActivity
LIB=tawara_mobile
MIN_API=26
TARGET_API=35
TARGETS=(aarch64-linux-android x86_64-linux-android)
IMG="system-images;android-35;google_apis;x86_64"
NDK_PIN=29.0.14206865
SERIAL=emulator-5554
TAG=RustStdoutStderr
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
OUT=${RUNNER_TEMP:-/tmp}/tawara-android
SDK=${ANDROID_HOME:-${ANDROID_SDK_ROOT:?ANDROID_HOME is not set}}
export PATH="$SDK/platform-tools:$SDK/emulator:$SDK/cmdline-tools/latest/bin:$PATH"
LOG=$OUT/logcat.txt
mkdir -p "$OUT"

group() { echo "::group::$*"; }
endgroup() { echo "::endgroup::"; }
die() { echo "::error::$*"; exit 1; }

ndk_paths() {
  NDK=$(ls -d "$SDK"/ndk/29.* 2>/dev/null | sort -V | tail -1 || true)
  if [ -z "$NDK" ]; then
    # `yes` dies of SIGPIPE when sdkmanager exits; only sdkmanager's status counts.
    (yes 2>/dev/null || true) | sdkmanager --install "ndk;$NDK_PIN" >/dev/null
    NDK=$SDK/ndk/$NDK_PIN
  fi
  TC=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin
  [ -x "$TC/clang" ] || die "no NDK clang at $TC"
}

abi_of() {
  case $1 in
    aarch64-linux-android) echo arm64-v8a ;;
    x86_64-linux-android) echo x86_64 ;;
  esac
}

# --------------------------------------------------------------------- build
build() {
  ndk_paths
  echo "NDK $NDK"
  cd "$ROOT"
  for t in "${TARGETS[@]}"; do
    local u=${t//-/_}
    local U=${u^^}
    export "CARGO_TARGET_${U}_LINKER=$TC/${t}${MIN_API}-clang"
    export "CC_${u}=$TC/${t}${MIN_API}-clang" "CXX_${u}=$TC/${t}${MIN_API}-clang++"
    export "AR_${u}=$TC/llvm-ar" "RANLIB_${u}=$TC/llvm-ranlib"
    group "cargo rustc --crate-type cdylib --target $t"
    cargo rustc --locked --release -p tawara-mobile --lib --crate-type cdylib --target "$t"
    endgroup
    local so=target/$t/release/lib$LIB.so
    "$TC/llvm-readelf" --dyn-syms -W "$so" | grep -qE ' ANativeActivity_onCreate$' \
      || die "$so does not export ANativeActivity_onCreate"
    # 16 KB pages: every LOAD segment aligned to at least 2**14.
    "$TC/llvm-readelf" -lW "$so" | awk '$1=="LOAD"{print $NF}' | sort -u | while read -r align; do
      [ $((align)) -ge 16384 ] || die "$so: a LOAD segment aligned to $align"
    done
    ls -la "$so"
  done
}

# ----------------------------------------------------------------------- apk
apk() {
  ndk_paths
  cd "$ROOT"
  local bt jar w=$OUT/apk
  bt=$(ls -d "$SDK"/build-tools/* | sort -V | tail -1)
  jar=$(ls -d "$SDK"/platforms/android-3[5-9] 2>/dev/null | sort -V | tail -1)/android.jar
  [ -f "$jar" ] || die "no android-35 or later platform in $SDK/platforms"
  echo "build-tools $bt; platform $jar"
  rm -rf "$w"
  mkdir -p "$w/root/lib" "$w/res"
  for t in "${TARGETS[@]}"; do
    local abi
    abi=$(abi_of "$t")
    mkdir -p "$w/root/lib/$abi"
    "$TC/llvm-strip" --strip-debug -o "$w/root/lib/$abi/lib$LIB.so" "target/$t/release/lib$LIB.so"
  done
  # A throwaway key for this run only: the APK is for the emulator and for
  # the owner's device check, never for distribution.
  keytool -genkeypair -noprompt -keystore "$w/debug.keystore" -storepass android -keypass android \
    -alias androiddebugkey -keyalg RSA -keysize 2048 -validity 10000 \
    -dname "CN=Android Debug,O=Android,C=US" >/dev/null 2>&1
  "$bt/aapt2" compile --dir platform/android/res -o "$w/res"
  # Debuggable, so that the checks can look inside the app's directory
  # with run-as; a release build (phase 5) is not.
  "$bt/aapt2" link -o "$w/unsigned.apk" -I "$jar" --manifest platform/android/AndroidManifest.xml \
    -R "$w"/res/*.flat --min-sdk-version "$MIN_API" --target-sdk-version "$TARGET_API" --debug-mode \
    --version-code 1 --version-name "$(cargo metadata --no-deps --format-version 1 \
      | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="tawara-mobile"))')"
  # Stored, not deflated, and 16 KB-aligned: the manifest says
  # extractNativeLibs="false", so the system maps the libraries in place.
  (cd "$w/root" && zip -q -0 -r -D -X "../unsigned.apk" lib)
  "$bt/zipalign" -f -P 16 4 "$w/unsigned.apk" "$w/aligned.apk"
  "$bt/apksigner" sign --ks "$w/debug.keystore" --ks-pass pass:android --key-pass pass:android \
    --ks-key-alias androiddebugkey --v4-signing-enabled false --out "$OUT/tawara.apk" "$w/aligned.apk"
  "$bt/apksigner" verify "$OUT/tawara.apk"
  "$bt/zipalign" -c -P 16 4 "$OUT/tawara.apk"
  "$bt/aapt2" dump badging "$OUT/tawara.apk" | grep -E "^package:|targetSdk|native-code|uses-permission"
  "$bt/aapt2" dump xmltree --file AndroidManifest.xml "$OUT/tawara.apk" >"$OUT/manifest.txt"
  ls -la "$OUT/tawara.apk"
}

# ------------------------------------------------------------------ helpers
a() { adb -s "$SERIAL" "$@"; }
ash() { adb -s "$SERIAL" shell "$@" | tr -d '\r'; }

record() { # record NAME PASS|FAIL DETAIL
  printf '%s\t%s\t%s\n' "$1" "$2" "${3:-}" >>"$OUT/results.tsv"
  echo "CHECK $2 $1: ${3:-}"
}
check() { # check NAME DETAIL COMMAND...: PASS when the command succeeds
  local name=$1 detail=$2
  shift 2
  if "$@"; then record "$name" PASS "$detail"; else record "$name" FAIL "$detail"; fi
}
mark() { wc -l <"$LOG"; }
app_lines() { tail -n +"$(($1 + 1))" "$LOG" | grep -E "$TAG: TAWARA " | sed -E "s/.*$TAG: //" || true; }
wait_log() { # wait_log MARK ERE SECONDS: prints the first matching application line
  local t=0 line
  while :; do
    line=$(app_lines "$1" | grep -E -m1 "$2" || true)
    [ -n "$line" ] && { echo "$line"; return 0; }
    [ "$t" -ge "$3" ] && return 1
    sleep 1
    t=$((t + 1))
  done
}
app_pid() { ash pidof "$PKG" || true; }
# same_pid PID: the application runs under PID. An unknown PID never passes, so
# a process that is already gone cannot make the check pass.
same_pid() { [ -n "$1" ] && [ "$(app_pid)" = "$1" ]; }
resumed() { ash dumpsys activity activities | grep -m1 -E 'topResumedActivity=|ResumedActivity:' || true; }
crashed() { grep -E "FATAL EXCEPTION|Fatal signal|$TAG: thread .* panicked" "$LOG" >/dev/null 2>&1; }
wait_sys() { # wait_sys MARK ERE SECONDS: prints the first line since MARK that matches, of any tag
  local t=0 line
  while :; do
    line=$(tail -n +"$(($1 + 1))" "$LOG" | grep -E -m1 "$2" || true)
    [ -n "$line" ] && { echo "$line"; return 0; }
    [ "$t" -ge "$3" ] && return 1
    sleep 1
    t=$((t + 1))
  done
}
# no_sys MARK ERE: no line since MARK matches. grep counts every line rather
# than stop at the first, so that tail is never cut off by a closed pipe, which
# pipefail would report as a failure.
no_sys() {
  local n
  n=$(tail -n +"$(($1 + 1))" "$LOG" | grep -cE "$2" || true)
  [ "$n" = 0 ]
}
overlay_state() { ash cmd overlay list | grep -E "\] $1\$" | head -n 1 || true; }
# trace MARK [END]: the log's lines after MARK, up to END or to the end of the
# log, that say how the application started and how it ended, each cut to 300
# characters. When more than 200 are kept, the last 200 are printed with a line
# saying how many were left out, since a relaunch and a process end come last.
# The lines kept are the system's records that name $PKG; the overlay commits,
# which are the system's record of an overlay change and name no application;
# and, from the application's own processes (those that log $PKG or that the
# system starts for it), their activity events, asset updates, fatal messages
# and TAWARA lines. Other applications' lines are left out. The overlay list
# and some checks' details still name the image's own system packages.
trace() {
  local slice=$OUT/trace-slice.txt kept=$OUT/trace-kept.txt n
  awk -v a="$1" -v b="${2:-}" 'NR > a && (b == "" || NR <= b)' "$LOG" >"$slice" || true
  if ! awk -v pkg="$PKG" '
    FNR == NR {
      if (index($0, pkg)) {
        if ($6 ~ /^(RustStdoutStderr|ActivityThread|AndroidRuntime):$/) own[$3] = 1
        if (match($0, /am_proc_(start|died): \[[0-9]+,[0-9]+,/)) {
          split(substr($0, RSTART, RLENGTH), f, ",")
          own[f[2]] = 1
        }
      }
      next
    }
    {
      tag = $6
      sub(/:$/, "", tag)
      event = (tag ~ /^(am|wm)_/)
      named = (tag ~ /^(ActivityTaskManager|ActivityManager|WindowManager|ActivityThread|AndroidRuntime|DEBUG|lowmemorykiller|OverlayManager|PackageManager)$/)
      if (index($0, pkg) && (event || named)) print
      else if (tag == "commit_sys_config_file" && /\[overlays,/) print
      else if (!($3 in own)) next
      else if (event) print
      else if (tag == "ActivityThread" && /assets (added|removed)/) print
      else if (tag == "libc" && $5 == "F") print
      else if (tag == "RustStdoutStderr" && / RustStdoutStderr: TAWARA /) print
    }
  ' "$slice" "$slice" >"$kept"; then
    echo "::warning::trace: awk failed, so the lines below may be incomplete"
  fi
  n=$(awk 'END { print NR }' "$kept")
  if [ "$n" -gt 200 ]; then echo "($((n - 200)) earlier lines not shown)"; fi
  tail -n 200 "$kept" | cut -c1-300
  rm -f "$slice" "$kept"
}

set_ini() {
  local f=$1 k=$2 v=$3
  if grep -q "^$k=" "$f"; then sed -i "s|^$k=.*|$k=$v|" "$f"; else echo "$k=$v" >>"$f"; fi
}

emulator_up() {
  group "KVM"
  echo 'KERNEL=="kvm", GROUP="kvm", MODE="0666", OPTIONS+="static_node=kvm"' \
    | sudo tee /etc/udev/rules.d/99-kvm4all.rules >/dev/null
  sudo udevadm control --reload-rules
  sudo udevadm trigger --name-match=kvm
  ls -l /dev/kvm || die "/dev/kvm missing"
  endgroup

  group "SDK packages"
  (yes 2>/dev/null || true) | sdkmanager --licenses >/dev/null 2>&1 || true
  sdkmanager --install platform-tools emulator "$IMG" >"$OUT/sdkmanager.txt" 2>&1 \
    || { tail -30 "$OUT/sdkmanager.txt"; die "sdkmanager failed"; }
  # The emulator links against desktop libraries the runner image lacks;
  # -no-window still loads them.
  sudo apt-get update -q >/dev/null
  sudo apt-get install -y -q --no-install-recommends libpulse0 libnss3 libxcomposite1 \
    libxcursor1 libxdamage1 libxi6 libxtst6 libxkbfile1 libgl1 libegl1 libasound2t64 \
    libbsd0 libxkbcommon-x11-0 libx11-xcb1 >/dev/null
  emulator -version | head -1
  endgroup

  group "AVD and boot"
  echo no | avdmanager create avd --force -n tawara -k "$IMG"
  local avd_dir
  avd_dir=$(avdmanager list avd | sed -n 's/^ *Path: \(.*tawara\.avd\)$/\1/p' | head -1)
  [ -n "$avd_dir" ] && [ -f "$avd_dir/config.ini" ] || { avdmanager list avd; die "cannot find the AVD"; }
  export ANDROID_AVD_HOME
  ANDROID_AVD_HOME=$(dirname "$avd_dir")
  local cfg=$avd_dir/config.ini
  set_ini "$cfg" hw.keyboard no
  set_ini "$cfg" hw.ramSize 4096
  set_ini "$cfg" hw.cpu.ncore 4
  set_ini "$cfg" disk.dataPartition.size 6G
  set_ini "$cfg" hw.lcd.width 1080
  set_ini "$cfg" hw.lcd.height 2400
  set_ini "$cfg" hw.lcd.density 420
  nohup emulator -avd tawara -port "${SERIAL#emulator-}" -no-window -no-audio -no-boot-anim \
    -no-snapshot -gpu swiftshader_indirect -accel on -camera-back none -camera-front none \
    >"$OUT/emulator.txt" 2>&1 &
  echo $! >"$OUT/emulator.pid"
  adb start-server >/dev/null
  timeout 300 adb -s "$SERIAL" wait-for-device || { tail -50 "$OUT/emulator.txt"; die "no adb device"; }
  local t=0
  until [ "$(ash getprop sys.boot_completed 2>/dev/null)" = 1 ]; do
    kill -0 "$(cat "$OUT/emulator.pid")" 2>/dev/null || { tail -80 "$OUT/emulator.txt"; die "the emulator exited"; }
    [ "$t" -ge 600 ] && { tail -80 "$OUT/emulator.txt"; die "no boot after 600 s"; }
    sleep 5
    t=$((t + 5))
  done
  until a shell pm path android >/dev/null 2>&1; do sleep 2; done
  echo "booted after about ${t} s"
  endgroup

  group "device"
  ash settings put global window_animation_scale 0
  ash settings put global transition_animation_scale 0
  ash settings put global animator_duration_scale 0
  ash settings put global stay_on_while_plugged_in 7
  ash settings put system screen_off_timeout 2147483647
  ash input keyevent KEYCODE_WAKEUP
  ash wm dismiss-keyguard || true
  echo "sdk=$(ash getprop ro.build.version.sdk) abi=$(ash getprop ro.product.cpu.abilist)"
  # The events buffer holds the records of the activity's starts, relaunches
  # and process ends (wm_*, am_*), which `trace` prints. Without it a relaunch
  # would leave no line, and a start that ends would be unexplained (D32 item 9
  # (i) and (d)).
  a logcat -b main,system,crash,events -c || true
  a logcat -b main,system,crash,events -v threadtime >"$LOG" 2>&1 &
  echo $! >"$OUT/logcat.pid"
  endgroup
}

emulator_down() {
  a emu kill >/dev/null 2>&1 || true
  [ -f "$OUT/logcat.pid" ] && kill "$(cat "$OUT/logcat.pid")" 2>/dev/null || true
}

# ------------------------------------------------------------------- checks
summary() {
  {
    echo "### Android checks"
    echo
    echo "| check | result | detail |"
    echo "|---|---|---|"
    while IFS=$'\t' read -r name result detail; do
      echo "| $name | $result | ${detail//|/\\|} |"
    done <"$OUT/results.tsv"
  } >>"${GITHUB_STEP_SUMMARY:-/dev/null}"
}

# Not `emulator`: a function of that name would shadow the SDK's emulator
# program, which emulator_up runs.
on_emulator() {
  # A run's results are its own: a FAIL left from an earlier run would fail
  # this one at the end whatever it found (Tawara-mobile#1's review).
  : >"$OUT/results.tsv"
  emulator_up
  trap 'summary; emulator_down' EXIT

  group "the manifest"
  # The backup rules (docs/PLAN.md section 4.5; D32 item 7).
  check manifest.allowBackup "android:allowBackup is false" \
    grep -qE 'allowBackup\(0x[0-9a-f]+\)=(false|\(type 0x12\)0x0)' "$OUT/manifest.txt"
  check manifest.dataExtractionRules "android:dataExtractionRules names the rules" \
    grep -q 'android:dataExtractionRules' "$OUT/manifest.txt"
  # The activity handles every configuration change this mask names (D32
  # item 9), so the mask must hold the eighteen names of API 35. A name left
  # out lets Android recreate the activity, which ends the application. The
  # detail shows each mask aapt2 read, to compare bit by bit with 0x5000ffff.
  # They are joined on one line, so a second element with a mask of its own
  # fails the check rather than hide behind the first.
  local changes
  changes=$(awk 'match($0, /configChanges\(0x[0-9a-f]+\)=.*/) {
    s = s (s == "" ? "" : "; ") substr($0, RSTART)
  } END { print (s == "" ? "no configChanges" : s) }' "$OUT/manifest.txt")
  check manifest.configChanges "want 0x5000ffff (18 names): $changes" \
    grep -qE '^configChanges\(0x[0-9a-f]+\)=(\(type 0x11\))?0x5000ffff$' <<<"$changes"
  endgroup

  group "install and start"
  # The mark before the install, so that the trace also holds the system's
  # overlay update, which can follow the install within a second (D32 item 9 (i)).
  local m m0 pid
  m0=$(mark)
  a install -r -g "$OUT/tawara.apk"
  m=$(mark)
  ash am start -W -n "$PKG/$ACTIVITY"
  check start "the application starts in its private directory" \
    wait_log "$m" '^TAWARA start: private directory .*/no_backup$' 60
  sleep 5
  pid=$(app_pid)
  check running "a process, pid $pid" test -n "$pid"
  check resumed "the activity is in front: $(resumed)" grep -q "$PKG" <<<"$(resumed)"
  endgroup

  group "first launch: the system's account"
  # Printed in every run, so that a start which ends explains itself: the
  # system's lines since before the install, and the overlays it had then. The
  # account at the end of the run starts where this one stops.
  local mt
  mt=$(mark)
  trace "$m0" "$mt"
  ash cmd overlay list | head -n 200 || true
  endgroup

  group "FLAG_SECURE"
  # The window's flags (D32 item 4): dumpsys lists FLAG_SECURE as SECURE.
  local flags
  flags=$(ash dumpsys window windows | grep -A30 "$PKG/$ACTIVITY" | grep -m1 -E 'fl=|flags=' || true)
  check flag_secure "${flags//$'\t'/ }" grep -qE 'SECURE' <<<"$flags"
  endgroup

  group "the store's directory"
  # no_backup/ is the store's parent (D21). android.rs makes it mode 0700,
  # since the library refuses a parent as open as files/, which is 0771
  # (docs/spikes/P1-REPORT.md, A19). The mode must be exactly that: testing
  # only the others' bits would pass 0771 or 0750, which let the group in.
  # toybox's ls prints the mode and then a space.
  local listing
  listing=$(ash run-as "$PKG" ls -ld no_backup 2>&1 || true)
  check no_backup.private "$listing" grep -qE '^drwx------ ' <<<"$listing"
  endgroup

  group "Home: suspended and locked, then back"
  m=$(mark)
  ash input keyevent KEYCODE_HOME
  check home.locks "the lock on leaving the screen" \
    wait_log "$m" '^TAWARA lifecycle: suspended; locking$' 30
  sleep 2
  ash am start -W -n "$PKG/$ACTIVITY"
  sleep 5
  check home.same_process "the process lives on (pid $(app_pid), was $pid)" same_pid "$pid"
  check home.resumed "in front again: $(resumed)" grep -q "$PKG" <<<"$(resumed)"
  endgroup

  group "Back: to the background, not finished"
  m=$(mark)
  ash input keyevent KEYCODE_BACK
  check back.locks "Back takes the task to the background, which locks" \
    wait_log "$m" '^TAWARA lifecycle: suspended; locking$' 30
  sleep 2
  ash am start -W -n "$PKG/$ACTIVITY"
  sleep 5
  check back.same_process "the process lives on (pid $(app_pid), was $pid)" same_pid "$pid"
  endgroup

  group "an asset-path change keeps the activity"
  # The navigation mode is a framework overlay. Toggling it changes the asset
  # paths of every running application, and the system reports that to each of
  # them. An activity that does not declare assetsPaths is relaunched for the
  # change, and a relaunch ends this application (D32 item 9 (i) and (d)). The
  # check needs the activity in front before the toggle, so that a process which
  # is already gone cannot pass it. The update must then reach the application,
  # or the checks after it would pass without the change they are about. The
  # process must stay, the activity must stay in front and not be relaunched,
  # and the application must still lock when it leaves the screen. The trace is
  # taken before the overlay is put back, so that it holds the toggle alone.
  # The image must have the gestural navigation overlay; one without it fails
  # here, with nothing toggled.
  local ov=com.android.internal.systemui.navbar.gestural was op undo p0 front m1 m2 toggled
  was=$(overlay_state "$ov")
  p0=$(app_pid)
  front=$(resumed)
  if [ -z "$was" ]; then
    record assets.same_process FAIL "no overlay $ov on this image, so nothing was toggled"
  elif [ -z "$p0" ] || ! grep -q "$PKG" <<<"$front"; then
    record assets.same_process FAIL "not running and in front before the toggle (pid '$p0'), so nothing was toggled: $front"
    echo "The system's account since the first launch, at the end of this log, says why."
  else
    case $was in
      *'[x]'*) op=disable undo=enable ;;
      *) op=enable undo=disable ;;
    esac
    m1=$(mark)
    toggled=$(ash cmd overlay "$op" --user 0 "$ov" 2>&1 | tr '\t\n' '  ' || true)
    check assets.delivered "the system updated the application's asset paths after overlay $op${toggled:+ ($toggled)}" \
      wait_sys "$m1" "ActivityThread: ApplicationInfo updating for $PKG" 20
    sleep 10
    check assets.same_process "pid $(app_pid), was $p0, after overlay $op" same_pid "$p0"
    check assets.resumed "in front after overlay $op: $(resumed)" grep -q "$PKG" <<<"$(resumed)"
    check assets.no_relaunch "no relaunch of the activity after overlay $op" \
      no_sys "$m1" "wm_relaunch(_resume)?_activity: .*$PKG"
    m2=$(mark)
    trace "$m1" "$m2"
    ash input keyevent KEYCODE_HOME
    check assets.locks "the lock on leaving the screen after overlay $op" \
      wait_log "$m2" '^TAWARA lifecycle: suspended; locking$' 30
    ash cmd overlay "$undo" --user 0 "$ov" >/dev/null 2>&1 || true
    local t=0
    until [ "$(overlay_state "$ov")" = "$was" ] || [ "$t" -ge 10 ]; do
      sleep 1
      t=$((t + 1))
    done
    if [ "$(overlay_state "$ov")" = "$was" ]; then
      echo "overlay $ov put back: $was"
    else
      echo "::warning::overlay $ov not put back after overlay $undo: $(overlay_state "$ov"), was $was"
    fi
  fi
  endgroup

  if crashed; then
    record no_crash FAIL "$(grep -m3 -E 'FATAL|Fatal signal|panicked' "$LOG" | tr '\n' ' ')"
  else
    record no_crash PASS "no crash or panic in the log"
  fi

  group "the system's account since the first launch"
  trace "$mt"
  endgroup

  group "the application's lines"
  grep -E "$TAG: TAWARA " "$LOG" | sed -E "s/.*$TAG: //" || true
  endgroup
  ! grep -q $'\tFAIL\t' "$OUT/results.tsv"
}

case ${1:-} in
  build) build ;;
  apk) apk ;;
  emulator) on_emulator ;;
  *) die "usage: $0 build|apk|emulator" ;;
esac
