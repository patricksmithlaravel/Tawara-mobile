# Android platform glue

Tawara on Android is `crates/mobile` loaded as `libtawara_mobile.so` by
`android.app.NativeActivity`: no Java, Kotlin or Gradle (docs/DECISIONS.md
D4, D32). `applicationId`: `com.patricksmithlaravel.tawara` (D8).

- `AndroidManifest.xml`: NativeActivity, `hasCode="false"`, uncompressed
  16 KB-aligned native libraries, `INTERNET`, `CAMERA` for the QR scanner
  (asked for at run time, D42; `android.hardware.camera` and
  `android.hardware.camera.any` are not required, so a device with no camera
  installs it),
  the twenty configuration changes handled in place (D32 item 9: the
  eighteen of API 35, and `assetsPaths` and `resourcesUnused` of API 36, so
  that `configChanges` reads `0xd800ffff`), Back delivered as a key (phase 1,
  A14), and the backup rules: `android:allowBackup="false"` and
  `dataExtractionRules` (docs/PLAN.md section 4.5).
- `res/xml/data_extraction_rules.xml`: every domain excluded from cloud
  backup and from device transfer. The store lives in `no_backup/` besides
  (D21).
- `tawara.sh`: `build` (the library for arm64-v8a and x86_64, the release
  profile, with the NDK pinned in the script and the host's prebuilt
  toolchain), `apk` (linked with aapt2 against API 36's platform jar, which
  names `assetsPaths`, and targeting API 36 (`TARGET_API`, D32 item 9 (i),
  PR B); the platform is installed with sdkmanager when missing, and with the
  build number as its `versionCode`, from `platform/build-number.sh`;
  aligned, signed with a throwaway debug key: for the emulator and the
  owner's device check, never for distribution), `emulator` (the checks
  CI runs, `.github/workflows/mobile.yml`, and the system's lines about the
  application after the first start, after the asset-path check and at the
  end of the run, so that a start which ends says why) and `selftest` (the
  crash, abort and destroy parsers on logged lines, with no SDK or device;
  CI runs it before the emulator). The emulator has no camera
  (`-camera-back none -camera-front none`), so a scan there is refused at once,
  and the camera itself is not exercised: that waits for the device spike
  (D42).
- `release.sh`: the App Bundle for Google Play, `Tawara-X.Y.Z+N.aab`, as
  Tawara-wallet's docs/RELEASE.md section 6.4 gives it (D36). It builds both
  libraries with `tawara.sh build`, links the manifest and resources with
  build-tools 36.0.0's `aapt2 link --proto-format` against
  `platforms/android-36`, with `--target-sdk-version 36`, the build number as
  `versionCode` and the version as `versionName`, and never `--debug-mode`.
  bundletool 1.18.3 is fetched once into `~/.cache/tawara-release` (or
  `TAWARA_RELEASE_CACHE`), checked against its SHA-256 each time it is used,
  and builds and validates the bundle with 16 KB page alignment for the
  native libraries. The upload key signs it with `jarsigner`, and no
  `apksigner` is used, since it cannot sign a bundle. The script never
  uploads. Its inputs, all from the environment:
  - `ANDROID_HOME` with the pinned pieces, which are checked and never
    installed: build-tools 36.0.0, `platforms/android-36` and NDK
    29.0.14206865;
  - `JAVA_HOME`, a JDK 17 or later;
  - `TAWARA_BUILD_NUMBER`, the committed counter (required for a release);
  - `TAWARA_ANDROID_ICON_DIR`, a resource folder with the `mipmap-*`
    `ic_launcher` files that the manifest names as `@mipmap/ic_launcher`
    (required for a release);
  - `TAWARA_UPLOAD_KEYSTORE`, `TAWARA_UPLOAD_KEY_ALIAS`, and
    `TAWARA_UPLOAD_STOREPASS` (or a prompt on a terminal), with
    `TAWARA_UPLOAD_KEYPASS` when the key's password differs; without them the
    script says what is unset, signs nothing and exits 0;
  - `TAWARA_ANDROID_APK=1` for a universal APK for sideloading, signed with
    the same key;
  - `TAWARA_ANDROID_DRY_RUN=1` builds and checks everything and turns the
    release-only inputs into warnings;
  - `TAWARA_ANDROID_OUT`, where the outputs go (`target/android-release`, or
    `target/android-dry-run` for a dry run).
  It writes `SHA256SUMS` beside the bundle. It checks the bundle's manifest
  (target 36, the build number, not debuggable, backup off, the backup rules
  named, `configChanges` `0xd800ffff`) and the notices under `assets/`, and
  the universal APK's signature, 16 KB alignment, libraries, notices and mask.
  Nothing it makes is uploaded, validated with Google or sent anywhere.

What the Rust side does (`crates/mobile/src/android.rs`): `FLAG_SECURE` on
the whole window (D32 item 4), the store in `no_backup/`, the clipboard
through JNI with every copy marked sensitive (D32 item 10), the share sheet (an
`ACTION_SEND` chooser) and Back through JNI, and the lock when the system
suspends the application. The QR scanner (`crates/mobile/src/android/scan.rs`,
D42) reads the back camera through Camera2 and an `AImageReader` from the NDK,
and decodes the luminance with `rqrr` (`crates/mobile/src/qr.rs`). It asks for
the camera permission through JNI and polls the answer, since a NativeActivity
gets no permission callback. It gives the application each frame's preview
through `preview_frame`, ten a second at most, turned upright for the display's
rotation, and the application draws it. Its camera stops when the scan ends,
when the application moves to the background, when the window loses its focus,
and when the application ends. The words it shows for a scan that cannot start
are provisional until the owner approves them (D42).

To try a build on a phone: take `tawara.apk` from a local
`platform/android/tawara.sh build && platform/android/tawara.sh apk` (with
`ANDROID_HOME` set and the NDK installed by it), then
`adb install -r tawara.apk`.
