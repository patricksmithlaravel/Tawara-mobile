#!/usr/bin/env bash
# Tawara for Android, with plain commands only (docs/DECISIONS.md D13, D32):
#
#   platform/android/tawara.sh build      # libtawara_mobile.so, arm64-v8a and x86_64
#   platform/android/tawara.sh apk        # the APK, signed with a throwaway debug key
#   platform/android/tawara.sh emulator   # boot an emulator, install, run the checks
#   platform/android/tawara.sh selftest   # the crash parser, on logged lines
#
# No Java, Kotlin or Gradle: the activity is android.app.NativeActivity, the
# APK is linked with aapt2 and zipped, aligned and signed by hand, as phase
# 1's spike was (Tawara-wallet's docs/spikes/P1-REPORT.md). A release key is
# phase 5's.
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
# crash_lines PID...: one line for each crash in the log since its start, as
# 'own' or 'other', a tab, and what the log said. A crash whose process is
# named is judged by that name alone: it is the application's when the name is
# the package or one of the package's processes, and another process's
# otherwise, even when its PID is one the application once had, because the
# recorded PIDs span the reboot and another process can be given one of them.
# A crash whose process is not named is the application's when its PID is one
# the run recorded or one that wrote a TAWARA line in this log. The PIDs the
# run recorded miss a process that was restarted, or one that died before its
# PID was read, but such a process still wrote its TAWARA lines under its PID
# if it got as far as its start. Any other unnamed crash is 'other', so that
# another process's crash is not taken for the application's.
#
# AndroidRuntime writes the FATAL EXCEPTION line and the line that names its
# process as one message, so the line right after a FATAL EXCEPTION belongs to
# it: it is 'Process: NAME, PID: N', or 'PID: N' for a process without a name.
# Without such a line, the PID column of the FATAL line stands in, because the
# crashing process writes that line itself. The system server's '*** FATAL
# EXCEPTION IN SYSTEM PROCESS' banner is matched the same way. A 'Fatal signal'
# gives the thread as 'tid N (thread)' and the crashing process as 'pid N
# (name)'. Its PID is read from there and not from the PID column, because the
# line can be written by another process than the one that crashed. Its name is
# the kernel's, which keeps 15 characters: the application's is 'hlaravel.tawara',
# the tail of com.patricksmithlaravel.tawara, on API 36 (D32 item 9 (j)). So the
# name is matched with the first or the last 15 characters of the package, and
# never with the whole package, which such a line does not hold. The first 15 are
# accepted too, since the logs here cannot show which 15 an older release keeps.
# A 'Fatal signal' that does not name its process is the exception to the rule
# above: it is counted as the application's, since nothing shows that it belongs
# to another process, and exit.no_sigabrt catches only the application's aborts,
# and only from the first Back on. A Rust panic under the application's own tag
# is the application's by that tag.
crash_lines() {
  awk -v pkg="$PKG" -v tag="$TAG" -v pids=" $* " '
    BEGIN {
      n = 0; pending = 0; panic = tag ": thread .* panicked"
      # The kernel name of the process: 15 characters (see above).
      long = length(pkg) > 15
      head = long ? substr(pkg, 1, 15) : pkg
      tail = long ? substr(pkg, length(pkg) - 14) : pkg
    }
    index($0, tag ": TAWARA ") { app[$3] = 1 }
    index($0, "FATAL EXCEPTION") {
      pending = ++n
      text[n] = substr($0, index($0, "FATAL EXCEPTION"))
      name[n] = ""
      pid[n] = $3
      where[n] = ", pid " $3 ", no process name logged"
      unknown[n] = 0
      next
    }
    pending && match($0, /(Process: [^,]+, )?PID: [0-9]+/) {
      s = substr($0, RSTART, RLENGTH)
      i = index(s, "PID: ")
      pid[pending] = substr(s, i + 5)
      if (i > 1) {
        name[pending] = substr(s, 10, i - 12)
        where[pending] = " in " name[pending] ", pid " pid[pending]
      } else {
        where[pending] = ", pid " pid[pending] ", no process name logged"
      }
      pending = 0
      next
    }
    index($0, "Fatal signal") {
      pending = 0
      text[++n] = substr($0, index($0, "Fatal signal"))
      name[n] = ""
      pid[n] = ""
      where[n] = ", no process logged"
      unknown[n] = 1
      if (match($0, /pid [0-9]+( \([^)]*\))?/)) {
        s = substr($0, RSTART + 4, RLENGTH - 4)
        i = index(s, " (")
        if (i > 0) {
          pid[n] = substr(s, 1, i - 1)
          name[n] = substr(s, i + 2, length(s) - i - 2)
          where[n] = ""
          unknown[n] = 0
        } else {
          pid[n] = s
          where[n] = ", no process name logged"
        }
      }
      next
    }
    match($0, panic) {
      pending = 0
      text[++n] = substr($0, index($0, tag))
      name[n] = pkg
      pid[n] = $3
      where[n] = ""
      unknown[n] = 0
      next
    }
    { pending = 0 }
    END {
      for (i = 1; i <= n; i++) {
        if (unknown[i] || name[i] == pkg || name[i] == head || name[i] == tail || index(name[i], pkg ":") == 1) mine = 1
        else if (name[i] != "") mine = 0
        else mine = (pid[i] != "" && ((pid[i] in app) || index(pids, " " pid[i] " ") > 0))
        s = text[i] where[i]
        printf "%s\t%s\n", (mine ? "own" : "other"), substr(s, 1, 240)
      }
    }
  ' "$LOG"
}
# no_crash_check PID...: records the no_crash check. A crash of the application
# fails it. A crash of another process is printed as a note and passes, since the
# emulator's own processes can crash while the application does nothing wrong. A
# native crash of the application still fails it, since exit.no_sigabrt covers
# only aborts, and only from the first Back on. An empty or missing log fails
# it, since a log with nothing in it cannot show that nothing crashed.
no_crash_check() {
  local crashes own other
  if [ ! -s "$LOG" ]; then
    record no_crash FAIL "the log is empty or missing, so a crash cannot be ruled out"
    return 0
  fi
  crashes=$(crash_lines "$@")
  own=$(grep '^own' <<<"$crashes" | cut -f2- | head -3 | tr '\n' ' ' || true)
  other=$(grep '^other' <<<"$crashes" | cut -f2- | head -1 || true)
  if [ -n "$own" ]; then
    record no_crash FAIL "$own"
  elif [ -n "$other" ]; then
    record no_crash PASS "no crash of the application; another process's crash is noted, not counted: $other"
  else
    record no_crash PASS "no crash or panic in the log"
  fi
  grep '^other' <<<"$crashes" | cut -f2- | sed 's/^/note: not counted, another process: /' || true
}
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
# application's; no_crash notes that abort without failing on it.
aborts() { # aborts MARK PID...
  local mark=$1
  shift
  tail -n +"$((mark + 1))" "$LOG" | grep -E 'Fatal signal 6|SIGABRT' \
    | awk -v pids=" $* " -v pkg="$PKG" 'index(pids, " " $3 " ") || index($0, pkg)' || true
}
# 'Don't keep activities' is the global setting always_finish_activities, and
# the framework reads it only when it starts: ActivityManagerService.systemReady
# calls retrieveSettings (AOSP android-15.0.0_r1), which sets the field that
# OomAdjuster checks before it asks for every activity to be destroyed. A later
# write to the setting changes the stored value and nothing else, so each change
# writes the setting and reboots the emulator, and the field is then read back
# with dumpsys (D32 item 9 (j)). The Developer Options switch sets the field
# through the binder method setAlwaysFinish, which this test does not call: a
# binder transaction number comes from the build's AIDL file, not from a
# published interface, so a script that calls one is tied to its image.
#
# KEEP_WAS is the value before the test first changed it: empty while untouched,
# `null` when it was never set. KEEP_CHANGED is yes from the first write until
# the value is back, so the EXIT trap puts it back once and no more.
KEEP_WAS=
KEEP_CHANGED=no

# keep_write 1|0|delete: writes the setting, then reboots so the framework reads
# it. Returns 1 when the emulator does not come back.
keep_write() {
  case $1 in
    delete) ash settings delete global always_finish_activities >/dev/null || return 1 ;;
    *) ash settings put global always_finish_activities "$1" || return 1 ;;
  esac
  # The settings provider writes a change after a delay of up to two seconds
  # (SettingsState.java, WRITE_SETTINGS_DELAY_MILLIS and its maximum), so the
  # reboot waits for the file first.
  sleep 3
  reboot_and_wait
}
# dont_keep_activities 1|0: turns the option on or off, through a reboot.
dont_keep_activities() {
  KEEP_CHANGED=yes
  keep_write "$1"
}
# Puts the option back as the test found it. Does nothing once that is done.
restore_keep() {
  local failed=0
  [ "$KEEP_CHANGED" = yes ] || return 0
  case $KEEP_WAS in
    '') return 0 ;;
    null) keep_write delete || failed=1 ;;
    *) keep_write "$KEEP_WAS" || failed=1 ;;
  esac
  if [ "$failed" -eq 0 ]; then KEEP_CHANGED=no; fi
  return "$failed"
}
# The EXIT trap's part: the option is put back, or the failure is said.
restore_at_exit() {
  restore_keep >/dev/null 2>&1 || echo "::error::'Don't keep activities' could not be put back to $KEEP_WAS"
}

# Reboots the emulator and waits until the framework answers again, then wakes
# the screen and starts a new logcat stream. Returns 1 when the emulator does
# not come back within about ten minutes.
reboot_and_wait() {
  local before="" after="" t=0
  before=$(ash cat /proc/sys/kernel/random/boot_id || true)
  # Without the old boot id, the old boot would pass for the new one, and the
  # wait could end before the reboot had begun.
  [ -n "$before" ] || return 1
  # adb may report the connection closing as an error, so the boot id, not the
  # status of the command, shows whether the reboot happened.
  a reboot || true
  until [ -n "$after" ] && [ "$after" != "$before" ]; do
    [ "$t" -lt 300 ] || return 1
    sleep 3
    t=$((t + 3))
    after=$(ash cat /proc/sys/kernel/random/boot_id || true)
  done
  until [ "$(ash getprop sys.boot_completed || true)" = 1 ]; do
    [ "$t" -lt 600 ] || return 1
    sleep 3
    t=$((t + 3))
  done
  until a shell pm path android >/dev/null 2>&1; do
    [ "$t" -lt 600 ] || return 1
    sleep 2
    t=$((t + 2))
  done
  ash input keyevent KEYCODE_WAKEUP || true
  ash wm dismiss-keyguard || true
  restart_log
}
# A reboot ends the logcat stream with its connection. The new stream appends to
# the same file, so the marks taken with mark still hold. A new stream starts at
# the beginning of the device's buffer, which after a reboot holds that boot's
# lines, so the buffer is cleared first, as emulator_up does before the first
# stream: no_crash and the SIGABRT check then read the application's window and
# not the system's start (D32 item 9 (j)).
restart_log() {
  if [ -f "$OUT/logcat.pid" ]; then kill "$(cat "$OUT/logcat.pid")" 2>/dev/null || true; fi
  a logcat -c || { echo "::error::the log could not be cleared after the reboot"; return 1; }
  a logcat -v threadtime >>"$LOG" 2>&1 &
  echo $! >"$OUT/logcat.pid"
}
# The count of the lines dumpsys prints for the framework's own field: one while
# the field is on, since ActivityManagerService prints the line only then.
runtime_keep() {
  ash dumpsys activity processes | grep -c '^  mAlwaysFinishActivities=true$' || true
}
# destroy_state PID: what the event log holds since the last clear, which is
# made just before Back. It prints 'destroyed' when the framework logged this
# component's activity destroyed with the reason always-finish and PID had not
# died before it, whether or not PID died afterwards, 'died' when PID died
# first, and 'none' otherwise. The fields follow the tags'
# definitions in AOSP android-15.0.0_r1: wm_destroy_activity is User, Token,
# Task ID, Component Name and Reason, and am_proc_died is User, PID, Process
# Name, OomAdj and ProcState. The component name is the activity's ComponentName
# in its short form: here it is com.patricksmithlaravel.tawara/
# android.app.NativeActivity, because that class name does not begin with the
# package name, so the short form does not shorten it.
destroy_state() {
  ash logcat -b events -d -v threadtime | awk -v pid="$1" -v comp="$PKG/$ACTIVITY" '
    state != "" { next }
    /wm_destroy_activity/ && match($0, /\[.*\]/) {
      n = split(substr($0, RSTART + 1, RLENGTH - 2), f, ",")
      if (n == 5 && f[4] == comp && f[5] == "always-finish") state = "destroyed"
      next
    }
    /am_proc_died/ && match($0, /\[.*\]/) {
      n = split(substr($0, RSTART + 1, RLENGTH - 2), f, ",")
      if (n >= 2 && f[2] == pid) state = "died"
    }
    END { print (state == "" ? "none" : state) }
  '
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
  trap 'restore_at_exit; summary; emulator_down' EXIT

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
  # these processes in this window fails too. A run counts toward the exit
  # checks only when the framework destroyed its activity: the event log must
  # hold the destroy, for this component and with the reason always-finish,
  # before the run's process died (D32 item 9 (j)). The process must also stay
  # the one the run measured: a process alive afterwards has that PID, or the
  # application started again in the window. Without that destroy, Back only
  # backgrounds the activity, and a process that stays alive would look like an
  # exit that did not happen. exit.destroyed counts the destroys on their own,
  # so that 'not destroyed' and 'destroyed, but the lock not done' fail under
  # different checks.
  local m0 mr kept runtime restored state alive runs=5 run rpid line suspended exit_line gone aborted
  local pids="" started=0 locked=0 finished=0 ended=0 destroyed=0
  m0=$(mark)
  ash input keyevent KEYCODE_BACK
  sleep 5
  echo "$TAG lines after Back from the foreground:"
  tag_lines "$m0"
  # The first process's window ends with the reboot below, and after it the
  # same PID may belong to another process, so its SIGABRT check is made now.
  aborted=$(aborts "$m0" "$pid")
  KEEP_WAS=$(ash settings get global always_finish_activities)
  dont_keep_activities 1 || die "the emulator did not come back after the reboot that turns 'Don't keep activities' on"
  mr=$(mark)
  kept=$(ash settings get global always_finish_activities)
  check exit.setting "'Don't keep activities' reads $kept, was $KEEP_WAS" test "$kept" = 1
  # The framework read the stored value at boot, so its field should now hold
  # it, and dumpsys shows whether it does before any run is measured. The runs
  # go on when it does not: exit.destroyed asks each run's event log for the
  # destroy on its own, so a run without one cannot pass, and the runs' lines
  # still show what Back did.
  runtime=$(runtime_keep)
  check exit.runtime "the framework's field is on after the reboot: $runtime line(s) of mAlwaysFinishActivities=true, 1 expected" \
    test "$runtime" = 1
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
    # Cleared before Back, so the destroy read after it is this run's own.
    ash logcat -b events -c || die "the event log could not be cleared"
    ash input keyevent KEYCODE_BACK
    suspended=no exit_line='no TAWARA exit line' gone=no
    wait_device "$rpid" '^TAWARA lifecycle: suspended; locking$' 30 >/dev/null && suspended=yes
    line=$(wait_device "$rpid" '^TAWARA exit: ' 30 || true)
    [ -n "$line" ] && exit_line=$line
    # Long enough for a SIGABRT in the exit to be logged after the exit line.
    sleep 10
    [ -z "$(app_pid)" ] && gone=yes
    state=no-process
    [ -n "$rpid" ] && state=$(destroy_state "$rpid" || true)
    # A process alive now must be the one this run measured: a new PID means the
    # application started again in this window, so the destroy is not its own.
    alive=$(app_pid)
    if [ -n "$alive" ] && [ "$alive" != "$rpid" ]; then state=restarted; fi
    [ "$state" = destroyed ] && destroyed=$((destroyed + 1))
    # Only a run whose activity was destroyed counts as an exit measurement.
    if [ "$state" = destroyed ]; then
      [ "$suspended" = yes ] && locked=$((locked + 1))
      [ "$suspended" = yes ] && [ "$exit_line" = 'TAWARA exit: lock done' ] && finished=$((finished + 1))
      [ "$gone" = yes ] && ended=$((ended + 1))
    fi
    echo "run $run: pid $rpid; activity $state; locking logged: $suspended; $exit_line; process ended: $gone"
    echo "$TAG lines after Back with 'Don't keep activities' on:"
    tag_lines "$m"
  done
  # Before the reboot that puts the option back, for the same reason as the
  # first process's check.
  # shellcheck disable=SC2086 # Split on purpose: one argument per pid.
  [ -z "$pids" ] || aborted=$(printf '%s\n%s\n' "$aborted" "$(aborts "$mr" $pids)")
  if ! restore_keep; then
    echo "::error::'Don't keep activities' could not be put back; the EXIT trap tries again"
  fi
  check exit.started "$started of $runs relaunched processes started" test "$started" = "$runs"
  check exit.destroyed "$destroyed of $runs runs: the event log holds the activity's destroy after Back (always-finish), and the run's process had not died before it" \
    test "$destroyed" = "$runs"
  check exit.locks "$locked of $runs runs logged the lock after Back (only destroyed runs count)" test "$locked" = "$runs"
  check exit.ended "$ended of $runs processes ended after Back (only destroyed runs count)" test "$ended" = "$runs"
  check exit.lock "$finished of $runs runs: TAWARA exit: lock done (only destroyed runs count)" test "$finished" = "$runs"
  aborted=$(printf '%s\n' "$aborted" | grep -v '^$' | tr '\n' ' ' | cut -c1-300 || true)
  check exit.no_sigabrt "${aborted:-no SIGABRT or Fatal signal 6 from the application since the first Back}" \
    test -z "$aborted"
  restored=$(ash settings get global always_finish_activities || true)
  check exit.restored "'Don't keep activities' reads $restored, as it was ($KEEP_WAS)" test "$restored" = "$KEEP_WAS"
  endgroup

  # Only the application's crashes fail this check; see crash_lines.
  # shellcheck disable=SC2086 # Split on purpose: one argument per pid.
  no_crash_check "$pid" $pids

  group "the application's lines"
  grep -E "$TAG: TAWARA " "$LOG" | sed -E "s/.*$TAG: //" || true
  endgroup
  ! grep -q $'\tFAIL\t' "$OUT/results.tsv"
}

# selftest: the crash parser on the lines the emulator check logged in the run
# that reproduced the application's SIGABRT (D32 item 9 (j)). The five 'Fatal
# signal' lines are copied from that run's logcat: five processes, two thread
# names, the application's name as the kernel keeps it. Each must be counted as
# the application's. The system_server line is not from the run: it has the same
# form with another process's name, so that the other branch is exercised, and it
# must not be counted as the application's. A verdict that differs fails the script.
selftest() {
  local fixture=$OUT/selftest-logcat.txt saved=$LOG crashes own other failed=0
  cat >"$fixture" <<'LOGCAT'
10-12 23:45:29.642  2060  2132 F libc    : Fatal signal 6 (SIGABRT), code -1 (SI_QUEUE) in tid 2132 (hwuiTask0), pid 2060 (hlaravel.tawara)
10-12 23:45:47.758  3651  3676 F libc    : Fatal signal 6 (SIGABRT), code -1 (SI_QUEUE) in tid 3676 (hwuiTask0), pid 3651 (hlaravel.tawara)
10-12 23:46:06.085  4026  4051 F libc    : Fatal signal 6 (SIGABRT), code -1 (SI_QUEUE) in tid 4051 (hwuiTask0), pid 4026 (hlaravel.tawara)
10-12 23:46:22.372  4110  4135 F libc    : Fatal signal 6 (SIGABRT), code -1 (SI_QUEUE) in tid 4135 (hwuiTask0), pid 4110 (hlaravel.tawara)
10-12 23:46:39.773  4318  4345 F libc    : Fatal signal 6 (SIGABRT), code -1 (SI_QUEUE) in tid 4345 (hwuiTask1), pid 4318 (hlaravel.tawara)
10-12 23:45:29.600   655   695 F libc    : Fatal signal 6 (SIGABRT), code -6 (SI_TKILL) in tid 695 (system_server), pid 655 (system_server)
LOGCAT
  LOG=$fixture
  crashes=$(crash_lines 2060 3651 4026 4110 4318)
  LOG=$saved
  own=$(grep -c '^own' <<<"$crashes" || true)
  other=$(grep -c '^other' <<<"$crashes" || true)
  if [ "$own" = 5 ]; then
    echo "CHECK PASS selftest.app_aborts: $own of 5 of the application's aborts counted as its own"
  else
    echo "CHECK FAIL selftest.app_aborts: $own of 5 counted as the application's: $crashes"
    failed=1
  fi
  if [ "$other" = 1 ]; then
    echo "CHECK PASS selftest.other_process: another process's abort is not counted as the application's"
  else
    echo "CHECK FAIL selftest.other_process: $other counted as the application's, 1 expected: $crashes"
    failed=1
  fi
  return "$failed"
}

case ${1:-} in
  build) build ;;
  apk) apk ;;
  emulator) on_emulator ;;
  selftest) selftest ;;
  *) die "usage: $0 build|apk|emulator|selftest" ;;
esac
