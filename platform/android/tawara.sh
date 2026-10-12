#!/usr/bin/env bash
# Tawara for Android, with plain commands only (docs/DECISIONS.md D13, D32):
#
#   platform/android/tawara.sh build      # libtawara_mobile.so, arm64-v8a and x86_64
#   platform/android/tawara.sh apk        # the APK, signed with a throwaway debug key
#   platform/android/tawara.sh emulator   # boot an emulator, install, run the checks
#
# No Java, Kotlin or Gradle: the activity is android.app.NativeActivity, the
# APK is linked with aapt2 and zipped, aligned and signed by hand, as phase
# 1's spike was (docs/spikes/P1-REPORT.md). A release key is phase 5's.
#
# The application prints `TAWARA ...` lines on stderr, which android-activity
# sends to logcat under the tag RustStdoutStderr; the exit line it writes to
# logcat directly, under the same tag. This script prints
# `CHECK PASS|FAIL <name>: <detail>` lines and a table in the step summary,
# and fails if any check failed.
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
resumed() { ash dumpsys activity activities | grep -m1 -E 'topResumedActivity=|ResumedActivity:' || true; }
crashed() { grep -E "FATAL EXCEPTION|Fatal signal|$TAG: thread .* panicked" "$LOG" >/dev/null 2>&1; }
tag_lines() { tail -n +"$(($1 + 1))" "$LOG" | grep -E "$TAG" || true; } # since MARK
# The application's lines one process logged, read from the device's own
# buffer: the exit line is the last thing the process writes, so it should
# not depend on adb having written the file yet.
device_lines() { # device_lines PID
  ash logcat -d -v threadtime --pid="$1" | grep -E "$TAG: TAWARA " | sed -E "s/.*$TAG: //" || true
}
wait_device() { # wait_device PID ERE SECONDS: prints the first matching line of that process
  local t=0 line
  while :; do
    line=$(device_lines "$1" | grep -E -m1 "$2" || true)
    [ -n "$line" ] && { echo "$line"; return 0; }
    [ "$t" -ge "$3" ] && return 1
    sleep 1
    t=$((t + 1))
  done
}
# A SIGABRT since MARK in one of the PIDs (logcat's third column) or naming
# the package, so that another process's abort is not reported as the
# application's; the no_crash sweep still catches every process.
aborts() { # aborts MARK PID...
  local mark=$1
  shift
  tail -n +"$((mark + 1))" "$LOG" | grep -E 'Fatal signal 6|SIGABRT' \
    | awk -v pids=" $* " -v pkg="$PKG" 'index(pids, " " $3 " ") || index($0, pkg)' || true
}
# 'Don't keep activities', and its value before the checks changed it:
# empty while untouched, `null` when it was never set. The EXIT trap puts it
# back, so it is global, not the function's.
KEEP_WAS=
dont_keep_activities() { ash settings put global always_finish_activities "$1"; } # 1 on, 0 off
restore_keep() {
  case $KEEP_WAS in
    '') ;;
    null) ash settings delete global always_finish_activities >/dev/null ;;
    *) dont_keep_activities "$KEEP_WAS" ;;
  esac
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
  a logcat -c
  a logcat -v threadtime >"$LOG" 2>&1 &
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
  # 'Don't keep activities' is also put back here, so that a failure part-way
  # through the Back checks does not leave it changed (D32 item 9).
  trap 'restore_keep >/dev/null 2>&1 || true; summary; emulator_down' EXIT

  group "the manifest"
  # The backup rules (docs/PLAN.md section 4.5; D32 item 7).
  check manifest.allowBackup "android:allowBackup is false" \
    grep -qE 'allowBackup\(0x[0-9a-f]+\)=(false|\(type 0x12\)0x0)' "$OUT/manifest.txt"
  check manifest.dataExtractionRules "android:dataExtractionRules names the rules" \
    grep -q 'android:dataExtractionRules' "$OUT/manifest.txt"
  endgroup

  group "install and start"
  a install -r -g "$OUT/tawara.apk"
  local m pid
  m=$(mark)
  ash am start -W -n "$PKG/$ACTIVITY"
  check start "the application starts in its private directory" \
    wait_log "$m" '^TAWARA start: private directory .*/no_backup$' 60
  sleep 5
  pid=$(app_pid)
  check running "a process, pid $pid" test -n "$pid"
  check resumed "the activity is in front: $(resumed)" grep -q "$PKG" <<<"$(resumed)"
  endgroup

  group "FLAG_SECURE"
  # The window's flags (D32 item 4): dumpsys lists FLAG_SECURE as SECURE.
  local flags
  flags=$(ash dumpsys window windows | grep -A30 "$PKG/$ACTIVITY" | grep -m1 -E 'fl=|flags=' || true)
  check flag_secure "${flags//$'\t'/ }" grep -qE 'SECURE' <<<"$flags"
  endgroup

  group "the store's directory"
  # Not readable by other apps' users. The library makes the store's own
  # folder 0700 inside it when a store is written (D21).
  local listing
  listing=$(ash run-as "$PKG" ls -ld no_backup 2>&1 || true)
  check no_backup.private "$listing" grep -qE '^d.{6}-' <<<"$listing"
  endgroup

  group "Home: suspended and locked, then back"
  m=$(mark)
  ash input keyevent KEYCODE_HOME
  check home.locks "the lock on leaving the screen" \
    wait_log "$m" '^TAWARA lifecycle: suspended; locking$' 30
  sleep 2
  ash am start -W -n "$PKG/$ACTIVITY"
  sleep 5
  check home.same_process "the process lives on (pid $(app_pid), was $pid)" test "$(app_pid)" = "$pid"
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
  check back.same_process "the process lives on (pid $(app_pid), was $pid)" test "$(app_pid)" = "$pid"
  endgroup

  group "Back and the lock before the exit (D32 item 9)"
  # The process ends as soon as run_in returns, so the lock may not be done
  # by then. The check asks whether it is (D32 item 9). The first Back keeps
  # the activity, and the process lives on. With 'Don't keep activities' on,
  # Back destroys the activity as it leaves the screen. crates.io winit
  # 0.30.13 ignores that Destroy, so its loop does not end and the process
  # stays (exit.ended); the fork's release that D32 item 9 moves to ends the
  # loop on it, and the 'TAWARA exit' line then says whether the lock had
  # finished. Whether it has is a race between the interface taking the
  # worker's answer and the loop ending, so one run proves little: each of
  # the runs is a fresh process, and the checks give the count. A run counts
  # only if its own process logged the lock after Back, so that 'lock done'
  # is this move's lock and not a move with nothing to do. A SIGABRT from
  # these processes in this window fails too.
  local m0 kept runs=5 run rpid line suspended exit_line gone aborted
  local pids=$pid started=0 locked=0 finished=0 ended=0
  m0=$(mark)
  ash input keyevent KEYCODE_BACK
  sleep 5
  echo "$TAG lines after Back from the foreground:"
  tag_lines "$m0"
  KEEP_WAS=$(ash settings get global always_finish_activities)
  dont_keep_activities 1
  kept=$(ash settings get global always_finish_activities)
  check exit.setting "'Don't keep activities' reads $kept, was $KEEP_WAS" test "$kept" = 1
  for ((run = 1; run <= runs; run++)); do
    ash am force-stop "$PKG"
    m=$(mark)
    ash am start -W -n "$PKG/$ACTIVITY"
    if ! wait_log "$m" '^TAWARA start: ' 60 >/dev/null; then
      echo "run $run: the relaunched process did not start"
      continue
    fi
    started=$((started + 1))
    sleep 5
    rpid=$(app_pid)
    pids="$pids $rpid"
    m=$(mark)
    ash input keyevent KEYCODE_BACK
    suspended=no exit_line='no TAWARA exit line' gone=no
    wait_device "$rpid" '^TAWARA lifecycle: suspended; locking$' 30 >/dev/null && suspended=yes
    line=$(wait_device "$rpid" '^TAWARA exit: ' 30 || true)
    [ -n "$line" ] && exit_line=$line
    # Long enough for a SIGABRT in the exit to be logged after the exit line.
    sleep 10
    [ -z "$(app_pid)" ] && gone=yes
    [ "$suspended" = yes ] && locked=$((locked + 1))
    [ "$suspended" = yes ] && [ "$exit_line" = 'TAWARA exit: lock done' ] && finished=$((finished + 1))
    [ "$gone" = yes ] && ended=$((ended + 1))
    echo "run $run: pid $rpid; locking logged: $suspended; $exit_line; process ended: $gone"
    echo "$TAG lines after Back with 'Don't keep activities' on:"
    tag_lines "$m"
  done
  restore_keep
  check exit.started "$started of $runs relaunched processes started" test "$started" = "$runs"
  check exit.locks "$locked of $runs runs logged the lock after Back" test "$locked" = "$runs"
  check exit.ended "$ended of $runs processes ended after Back" test "$ended" = "$runs"
  check exit.lock "$finished of $runs runs: TAWARA exit: lock done" test "$finished" = "$runs"
  # shellcheck disable=SC2086 # Split on purpose: one argument per pid.
  aborted=$(aborts "$m0" $pids | tr '\n' ' ' | cut -c1-300)
  check exit.no_sigabrt "${aborted:-no SIGABRT or Fatal signal 6 from the application since the first Back}" \
    test -z "$aborted"
  endgroup

  if crashed; then
    record no_crash FAIL "$(grep -m3 -E 'FATAL|Fatal signal|panicked' "$LOG" | tr '\n' ' ')"
  else
    record no_crash PASS "no crash or panic in the log"
  fi

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
