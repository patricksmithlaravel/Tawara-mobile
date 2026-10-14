# Android platform glue

Tawara on Android is `crates/mobile` loaded as `libtawara_mobile.so` by
`android.app.NativeActivity`: no Java, Kotlin or Gradle (docs/DECISIONS.md
D4, D32). `applicationId`: `com.patricksmithlaravel.tawara` (D8).

- `AndroidManifest.xml`: NativeActivity, `hasCode="false"`, uncompressed
  16 KB-aligned native libraries, `INTERNET`, the nineteen configuration
  changes handled in place (D32 item 9: the eighteen of API 35 and
  `assetsPaths` of API 36), Back delivered as a key (phase 1, A14), and the
  backup rules: `android:allowBackup="false"` and `dataExtractionRules`
  (docs/PLAN.md section 4.5).
- `res/xml/data_extraction_rules.xml`: every domain excluded from cloud
  backup and from device transfer. The store lives in `no_backup/` besides
  (D21).
- `tawara.sh`: `build` (the library for arm64-v8a and x86_64), `apk` (linked
  with aapt2 against API 36's platform jar, which names `assetsPaths` and is
  installed with sdkmanager when missing, while the target stays at 35;
  aligned, signed with a throwaway debug key: for the emulator and the
  owner's device check, never for distribution), `emulator` (the checks
  CI runs, `.github/workflows/mobile.yml`, and the system's lines about the
  application after the first start, after the asset-path check and at the
  end of the run, so that a start which ends says why) and `selftest` (the
  crash, abort and destroy parsers on logged lines, with no SDK or device;
  CI runs it before the emulator).

What the Rust side does (`crates/mobile/src/android.rs`): `FLAG_SECURE` on
the whole window (D32 item 4), the store in `no_backup/`, the clipboard
through JNI with every copy marked sensitive (D32 item 10), the share sheet (an
`ACTION_SEND` chooser) and Back through JNI, and the lock when the system
suspends the application.

To try a build on a phone: take `tawara.apk` from a local
`platform/android/tawara.sh build && platform/android/tawara.sh apk` (with
`ANDROID_HOME` set and the NDK installed by it), then
`adb install -r tawara.apk`.
