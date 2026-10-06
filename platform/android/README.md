# Android platform glue

Tawara on Android is `crates/mobile` loaded as `libtawara_mobile.so` by
`android.app.NativeActivity`: no Java, Kotlin or Gradle (docs/DECISIONS.md
D4, D32). `applicationId`: `com.patricksmithlaravel.tawara` (D8).

- `AndroidManifest.xml`: NativeActivity, `hasCode="false"`, uncompressed
  16 KB-aligned native libraries, `INTERNET`, every configuration change
  handled in place and Back delivered as a key (phase 1, A18), and the
  backup rules: `android:allowBackup="false"` and `dataExtractionRules`
  (docs/PLAN.md section 4.5).
- `res/xml/data_extraction_rules.xml`: every domain excluded from cloud
  backup and from device transfer. The store lives in `no_backup/` besides
  (D21).
- `tawara.sh`: `build` (the library for arm64-v8a and x86_64), `apk` (linked
  with aapt2, aligned, signed with a throwaway debug key: for the emulator
  and the owner's device check, never for distribution) and `emulator` (the
  checks CI runs, `.github/workflows/mobile.yml`).

What the Rust side does (`crates/mobile/src/android.rs`): `FLAG_SECURE` on
the whole window (D32 item 4), the store in `no_backup/`, the clipboard and
Back through JNI, and the lock when the system suspends the application.

To try a build on a phone: take `tawara.apk` from a local
`platform/android/tawara.sh build && platform/android/tawara.sh apk` (with
`ANDROID_HOME` set and the NDK installed by it), then
`adb install -r tawara.apk`.
