#!/usr/bin/env bash
# Tawara for Android, with plain commands only (docs/DECISIONS.md D13, D32):
#
#   platform/android/tawara.sh build      # libtawara_mobile.so, arm64-v8a and x86_64
#   platform/android/tawara.sh apk        # the APK, signed with a throwaway debug key
#   platform/android/tawara.sh emulator   # boot an emulator, install, run the checks
#   platform/android/tawara.sh selftest   # the crash checks, on logged lines
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
# and fails if any check failed. A count that is reported and not gated is
# printed as `CHECK NOTE <name>: <detail>`: it is in the table, and it never
# fails the job (exit.lock; D32 item 9 (j)). After the first start, after the
# asset-path check and at the end of the run, it also prints the system's lines
# about the application's activity and process (`trace`), so that a start-up
# which ends says why in the job log.
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
# The logcat buffers the checks read: the application and the system, the
# crashes, and the events of activity starts, relaunches and process ends.
LOG_BUFFERS=main,system,crash,events
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
  # aapt2 links against API 36's jar, pinned rather than the newest one
  # installed, because it knows assetsPaths from that platform only (D32 item 9
  # (i)). The jar supplies the attribute names and the framework's resource
  # IDs, such as the theme's; the API level the application targets stays at
  # TARGET_API. A runner without the platform installs it, as ndk_paths does
  # the NDK, and the build stops if it is still missing rather than compile
  # against another platform.
  jar=$SDK/platforms/android-36/android.jar
  if [ ! -f "$jar" ]; then
    # `yes` dies of SIGPIPE when sdkmanager exits; only sdkmanager's status counts.
    (yes 2>/dev/null || true) | sdkmanager --install "platforms;android-36" >/dev/null
  fi
  [ -f "$jar" ] || die "no android-36 platform in $SDK/platforms (sdkmanager 'platforms;android-36')"
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

record() { # record NAME PASS|FAIL|NOTE DETAIL
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
# crash_lines FROM PID...: one line for each crash in the log after its line
# FROM (0 for the whole log), as 'own' or 'other', a tab, and what the log said.
# A crash whose process is named is judged by that name alone: it is the
# application's when the name is one the application's process goes by, and
# another process's otherwise, even when its PID is one the application once
# had, because the recorded PIDs span the reboot and another process can be
# given one of them. The manifest declares no android:process, so the
# application has one process, named after the package. A crash whose process
# is not named is the application's when its PID is one the run recorded or one
# that wrote a TAWARA line in this log. The PIDs the run recorded miss a process
# that was restarted, or one that died before its PID was read, but such a
# process still wrote its TAWARA lines under its PID if it got as far as its
# start. Any other unnamed crash is 'other', so that another process's crash is
# not taken for the application's.
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
# the kernel's, which keeps 15 characters. On the API 36 emulator the
# application's was 'hlaravel.tawara', the last 15 characters of
# com.patricksmithlaravel.tawara (D32 item 9 (j)), so such a name never holds
# the whole package. The name is matched with the package, with its last 15
# characters, and with each name the run read from /proc/PID/comm of the
# application's own processes (COMMS, see note_comm), so that an image that
# keeps other characters is matched by what it kept and not by a guess. Another
# package whose name ends in the same 15 characters would be taken for the
# application's, which can fail the check but never hide a crash.
# A 'Fatal signal' that does not name its process is the exception to the rule
# above: it is counted as the application's, since nothing shows that it belongs
# to another process. A Rust panic under the application's own tag is the
# application's by that tag.
crash_lines() {
  local from=$1
  shift
  awk -v pkg="$PKG" -v tag="$TAG" -v pids=" $* " -v comms=" $COMMS " -v from="$from" '
    BEGIN {
      n = 0; pending = 0; panic = tag ": thread .* panicked"
      # The kernel name of the process: 15 characters (see above).
      tail = length(pkg) > 15 ? substr(pkg, length(pkg) - 14) : pkg
    }
    index($0, tag ": TAWARA ") { app[$3] = 1 }
    # The lines up to FROM only say which PIDs wrote TAWARA lines.
    NR <= from { next }
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
        if (unknown[i] || name[i] == pkg || name[i] == tail || (name[i] != "" && index(comms, " " name[i] " "))) mine = 1
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
  crashes=$(crash_lines 0 "$@")
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
# aborts MARK PID...: the application's SIGABRTs since MARK, one a line. Whose
# a line is, crash_lines decides, as it does for no_crash, so the two checks
# cannot disagree: a 'Fatal signal' line is judged by the process it names, not
# by the process that wrote it. no_crash notes another process's abort without
# failing on it.
aborts() {
  crash_lines "$@" | awk -F '\t' '$1 == "own" && $2 ~ /Fatal signal 6|SIGABRT/ { print $2 }' || true
}
# COMMS: the names the kernel gave the application's processes in this run,
# separated by spaces, which crash_lines matches a crash's process name with.
COMMS=
# note_comm PID: adds the kernel's name for that process, from /proc/PID/comm,
# to COMMS. Called on each of the application's processes the run starts, while
# it lives, so that the crash checks match the name this image gives it.
note_comm() {
  local c
  [ -n "$1" ] || return 0
  c=$(ash cat "/proc/$1/comm" 2>/dev/null || true)
  [ -n "$c" ] || return 0
  case " $COMMS " in *" $c "*) ;; *) COMMS="${COMMS:+$COMMS }$c" ;; esac
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
# not the system's start (D32 item 9 (j)). It reads the buffers emulator_up reads,
# so that the events trace prints are in it too.
restart_log() {
  if [ -f "$OUT/logcat.pid" ]; then kill "$(cat "$OUT/logcat.pid")" 2>/dev/null || true; fi
  a logcat -b "$LOG_BUFFERS" -c || { echo "::error::the log could not be cleared after the reboot"; return 1; }
  a logcat -b "$LOG_BUFFERS" -v threadtime >>"$LOG" 2>&1 &
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
  # The events buffer holds the records of the activity's starts, relaunches
  # and process ends (wm_*, am_*), which `trace` prints. Without it a relaunch
  # would leave no line, and a start that ends would be unexplained (D32 item 9
  # (i) and (d)).
  a logcat -b "$LOG_BUFFERS" -c || true
  a logcat -b "$LOG_BUFFERS" -v threadtime >"$LOG" 2>&1 &
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
  # item 9), so the mask must hold the nineteen names: the eighteen of API 35
  # and assetsPaths of API 36. A name left out lets Android recreate the
  # activity, which ends the application. The detail shows each mask aapt2
  # read, to compare bit by bit with 0xd000ffff. They are joined on one line,
  # so a second element with a mask of its own fails the check rather than
  # hide behind the first.
  local changes
  changes=$(awk 'match($0, /configChanges\(0x[0-9a-f]+\)=.*/) {
    s = s (s == "" ? "" : "; ") substr($0, RSTART)
  } END { print (s == "" ? "no configChanges" : s) }' "$OUT/manifest.txt")
  check manifest.configChanges "want 0xd000ffff (19 names): $changes" \
    grep -qE '^configChanges\(0x[0-9a-f]+\)=(\(type 0x11\))?0xd000ffff$' <<<"$changes"
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
  note_comm "$pid"
  check running "a process, pid $pid, which the kernel names '${COMMS:-unread}'" test -n "$pid"
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

  group "an asset-path change keeps the activity"
  # The navigation mode is a framework overlay. Toggling it changes the asset
  # paths of every running application, and the system reports that to each of
  # them. An activity that does not declare assetsPaths is relaunched for the
  # change, and a relaunch ends this application (D32 item 9 (i) and (d)). The
  # manifest declares assetsPaths, so neither may happen here. The
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
    # Back in front for the Home and Back checks after this group, which begin
    # with the activity in front, as they did before it.
    ash am start -W -n "$PKG/$ACTIVITY" >/dev/null
    sleep 5
  fi
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
  # so that 'not destroyed' and 'destroyed, but the lock not done' are told
  # apart: the first fails exit.destroyed, and the second is reported on
  # exit.lock. exit.lock is a count, not a gate (D32 item 9 (j), owner,
  # 2026-10-13; Tawara-wallet #25). The lock's Leaving completes only inside
  # App::update, so a Destroy that comes within about 100 ms of going to the
  # background ends the process before the lock is done (1 in 15 local runs,
  # and on CI). Ending the process leaves no secret held by a running process,
  # and the library's store writes are atomic.
  local m0 mr kept runtime restored state alive runs=5 run rpid line suspended exit_line gone aborted
  local pids="" started=0 locked=0 finished=0 ended=0 destroyed=0
  m0=$(mark)
  ash input keyevent KEYCODE_BACK
  sleep 5
  echo "$TAG lines after Back from the foreground:"
  tag_lines "$m0"
  # The first process's window ends with the reboot below, so its SIGABRT
  # check is made now, before the reboot's own lines are in the log.
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
    note_comm "$rpid"
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
  # Reported, not gated (D32 item 9 (j)): a NOTE, which does not fail the job.
  # The other exit checks still gate.
  record exit.lock NOTE "$finished of $runs runs: TAWARA exit: lock done (only destroyed runs count; reported, not gated: D32 item 9 (j))"
  # The count first, since the lines themselves are cut short for the table.
  aborted=$(printf '%s\n' "$aborted" | grep -v '^$' || true)
  local naborts
  naborts=$(grep -c . <<<"$aborted" || true)
  if [ "$naborts" = 0 ]; then
    aborted="no SIGABRT or Fatal signal 6 from the application since the first Back"
  else
    aborted="$naborts of the application's aborts since the first Back: $(tr '\n' ' ' <<<"$aborted" | cut -c1-300)"
  fi
  check exit.no_sigabrt "$aborted" test "$naborts" = 0
  restored=$(ash settings get global always_finish_activities || true)
  check exit.restored "'Don't keep activities' reads $restored, as it was ($KEEP_WAS)" test "$restored" = "$KEEP_WAS"
  endgroup

  # Only the application's crashes fail this check; see crash_lines.
  # shellcheck disable=SC2086 # Split on purpose: one argument per pid.
  no_crash_check "$pid" $pids

  group "the system's account since the first launch"
  trace "$mt"
  endgroup

  group "the application's lines"
  grep -E "$TAG: TAWARA " "$LOG" | sed -E "s/.*$TAG: //" || true
  endgroup
  ! grep -q $'\tFAIL\t' "$OUT/results.tsv"
}

# selftest: the crash parser, no_crash and the abort check, on logged lines (D32
# item 9 (j)). The first five 'Fatal signal' lines are copied from the emulator
# run that reproduced the application's SIGABRT under std::process::exit: five
# processes, two thread names, the application's name as the kernel keeps it.
# The other three are made in the same form, since no run logged them: the
# system server's abort; an abort that names pid 1940 under the application's
# name but was written by pid 1000, so the PID column is not the crashing
# process; and an abort under the package's first 15 characters, which the
# kernel did not give the application in that run, so it counts as the
# application's only once the run has read that name from /proc. A result that
# differs fails the script.
selftest() {
  local dir=$OUT/selftest failed=0 got
  mkdir -p "$dir"
  # The functions below read LOG, OUT and COMMS. These stand in for the run's
  # own until this function returns, so no result of a run is touched.
  local LOG=$dir/logcat.txt
  local OUT=$dir
  local COMMS=
  : >"$OUT/results.tsv"
  cat >"$LOG" <<'LOGCAT'
10-12 23:45:29.642  2060  2132 F libc    : Fatal signal 6 (SIGABRT), code -1 (SI_QUEUE) in tid 2132 (hwuiTask0), pid 2060 (hlaravel.tawara)
10-12 23:45:47.758  3651  3676 F libc    : Fatal signal 6 (SIGABRT), code -1 (SI_QUEUE) in tid 3676 (hwuiTask0), pid 3651 (hlaravel.tawara)
10-12 23:46:06.085  4026  4051 F libc    : Fatal signal 6 (SIGABRT), code -1 (SI_QUEUE) in tid 4051 (hwuiTask0), pid 4026 (hlaravel.tawara)
10-12 23:46:22.372  4110  4135 F libc    : Fatal signal 6 (SIGABRT), code -1 (SI_QUEUE) in tid 4135 (hwuiTask0), pid 4110 (hlaravel.tawara)
10-12 23:46:39.773  4318  4345 F libc    : Fatal signal 6 (SIGABRT), code -1 (SI_QUEUE) in tid 4345 (hwuiTask1), pid 4318 (hlaravel.tawara)
10-12 23:46:40.600   655   695 F libc    : Fatal signal 6 (SIGABRT), code -6 (SI_TKILL) in tid 695 (system_server), pid 655 (system_server)
10-12 23:46:41.118  1000  1000 F libc    : Fatal signal 6 (SIGABRT), code -1 (SI_QUEUE) in tid 2007 (hwuiTask0), pid 1940 (hlaravel.tawara)
10-12 23:46:42.200  5000  5021 F libc    : Fatal signal 6 (SIGABRT), code -1 (SI_QUEUE) in tid 5021 (hwuiTask0), pid 5000 (com.patricksmit)
LOGCAT
  got=$(crash_lines 0 2060 3651 4026 4110 4318 | cut -f1 | tr '\n' ' ')
  selfcheck selftest.verdicts "own own own own own other own other " "$got" \
    "the five aborts of the run, the system server's, one written by another process, one under a name the run did not read"
  COMMS="hlaravel.tawara com.patricksmit"
  got=$(crash_lines 0 2060 | tail -1 | cut -f1)
  COMMS=
  selfcheck selftest.kernel_name own "$got" "an abort under a name read from /proc for the application"
  got="$(aborts 0 2060 | grep -c . || true) $(aborts 6 2060 | grep -c . || true)"
  selfcheck selftest.aborts "6 1" "$got" "the application's aborts in the whole log, and after line 6"
  no_crash_check 2060 >/dev/null
  got=$(tail -1 "$OUT/results.tsv" | cut -f2)
  LOG=$dir/other.txt
  sed -n 6p "$dir/logcat.txt" >"$LOG"
  no_crash_check 2060 >/dev/null
  got="$got $(tail -1 "$OUT/results.tsv" | cut -f2)"
  selfcheck selftest.no_crash "FAIL PASS" "$got" "no_crash on the whole log, and on the system server's abort alone"
  return "$failed"
}
# selfcheck NAME WANT GOT DETAIL: prints the selftest's CHECK line, and sets
# selftest's failed when GOT is not WANT.
selfcheck() {
  if [ "$3" = "$2" ]; then
    echo "CHECK PASS $1: $4: $3"
  else
    echo "CHECK FAIL $1: $4: got '$3', want '$2'"
    failed=1
  fi
}

case ${1:-} in
  build) build ;;
  apk) apk ;;
  emulator) on_emulator ;;
  selftest) selftest ;;
  *) die "usage: $0 build|apk|emulator|selftest" ;;
esac
