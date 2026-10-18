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
- `release.sh`: the App Bundle for Google Play, `Tawara-X.Y.Z+N.aab`, and an
  optional universal APK. Its steps, its environment and what the owner supplies
  are in "The Google Play bundle" below.

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

## The Google Play bundle

`release.sh` makes `Tawara-X.Y.Z+N.aab`, as Tawara-wallet's `docs/RELEASE.md`
section 6.4 describes, from the commit that is tagged for the upload (RELEASE.md
sections 5.1 to 5.4). It runs on the owner's machine, and CI never runs it (D13,
D36 item 4). D36 is proposed, and the script follows its recommendations, which
await the owner's decision.

It builds both libraries with `tawara.sh build`. It links the manifest and the
resources with build-tools 36.0.0's `aapt2 link --proto-format` against
`platforms/android-36`, with `--target-sdk-version 36`, the build number as
`versionCode`, the version as `versionName`, and never `--debug-mode`. The notices
go under the base module's `assets/`. bundletool 1.18.3 builds the bundle and
validates it with 16 KB page alignment for the native libraries, and `jarsigner`
signs it with the upload key, since `apksigner` cannot sign a bundle. The bundletool
jar is fetched once from its GitHub release, into `~/.cache/tawara-release` (or
`TAWARA_RELEASE_CACHE`), and checked against its SHA-256 each time it is used.
Apart from that download, nothing is sent anywhere, and nothing is uploaded or
validated with Google.

### Release steps

1. Settle what the owner supplies, listed below, and have the SDK pieces and a
   JDK 17 or later in place. The script checks for them and never installs them: a
   missing piece stops it, and the message gives the `sdkmanager` line that adds it.
2. Set the version, raise the build number, regenerate the notices, commit, and tag
   the commit `vX.Y.Z+N`, as steps 2 to 5 of the iOS list in `platform/ios/README.md`
   say. The same version and the same counter serve both stores.
3. Optional: a dry run, `TAWARA_ANDROID_DRY_RUN=1 platform/android/release.sh`. It
   builds and checks everything, treats the release-only inputs as warnings, and
   writes to `target/android-dry-run`.
4. Check out the tag, then run `platform/android/release.sh` with the variables below
   set, `TAWARA_BUILD_NUMBER=N` among them. The keystore password is typed at the
   prompt, or set in `TAWARA_UPLOAD_STOREPASS`.
5. Read the output. `SHA256SUMS` lists what was made, and the status lines say
   whether a signed bundle exists. Without a signature the bundle is named
   `Tawara-X.Y.Z+N-unsigned.aab`.
6. Upload the signed bundle by hand in Play Console, to the internal testing track,
   which enrols Play App Signing at the first upload (RELEASE.md sections 6.4, 8 and
   9).

The emulator checks on the universal APK, which RELEASE.md section 6.4 asks for, are
not in the script.

### Environment

None of these values is written into a file the repository keeps. Passwords reach
the tools through the environment or a private file that is removed on exit, never
on a command line.

- `ANDROID_HOME` (or `ANDROID_SDK_ROOT`): the SDK, with build-tools 36.0.0,
  `platforms/android-36` and NDK 29.0.14206865 (RELEASE.md section 1.6).
- `JAVA_HOME`: a JDK 17 or later, for bundletool and jarsigner. Without it, the
  `java` and `jarsigner` on the `PATH` are used. An older JDK stops the script.
- `TAWARA_BUILD_NUMBER`: required for a release, the committed counter (see the iOS
  list above). A dry run takes the development number.
- `TAWARA_ANDROID_ICON_DIR`: required for a release. A resource folder holding
  `mipmap-*/ic_launcher` files, which the manifest names as `@mipmap/ic_launcher`. A
  dry run without it leaves the icon out, with a warning.
- `TAWARA_UPLOAD_KEYSTORE` and `TAWARA_UPLOAD_KEY_ALIAS`: the upload keystore, and the
  alias of its key. Without either, or without a keystore password and a terminal to
  ask for one, the script names what is unset, signs nothing and exits 0, with the
  unsigned bundle in the output folder.
- `TAWARA_UPLOAD_STOREPASS`: the keystore's password, asked for on a terminal when it
  is unset.
- `TAWARA_UPLOAD_KEYPASS`: the key's password, when it is not the keystore's. It
  defaults to the keystore's.
- `TAWARA_ANDROID_APK=1`: also makes `Tawara-X.Y.Z+N.apk`, a universal APK for
  sideloading, signed with the same upload key. RELEASE.md section 6.4 names a
  throwaway key for its emulator run on that APK; the script does not use one.
- `TAWARA_ANDROID_DRY_RUN=1`: builds and checks everything, as step 3 says.
- `TAWARA_ANDROID_OUT`: where the outputs go; `target/android-release` by default.
- `TAWARA_RELEASE_CACHE`: where the bundletool jar is kept; `~/.cache/tawara-release`
  by default.

### What it checks

The bundle's manifest must target API 36, carry the build number, not be debuggable,
turn backup off, name the backup rules and have `configChanges` `0xd800ffff`. Its
configuration must ask for 16 KB pages, and the notices must be under `base/assets/`.
The universal APK, when it is made, must be signed, aligned for 16 KB pages, carry
the arm64-v8a and x86_64 libraries, the notices under `assets/`, and the same
version, target and mask. A failed check stops the script.

### What the owner supplies

None of these is in the repository or in CI (RELEASE.md section 1.2).

- A Google Play Console account, with identity verification (US$25 once), and the app
  for `com.patricksmithlaravel.tawara` (RELEASE.md section 1.2).
- An upload keystore, made with `keytool` and kept with an offline backup. Its path,
  its key alias and its password are the values above. Play App Signing holds the
  app's signing key once the first upload enrols it.
- The app icon: a resource folder with the `mipmap-*` `ic_launcher` files, which is
  Tawara's own mark, not the coin mark (RELEASE.md section 1.2; D27 item 2). The
  listing needs a 512-pixel version as well.
- The Android SDK pieces named above, and a JDK 17 or later (RELEASE.md section 1.6).
- In Play Console: the target API 36 rule, confirmed there (RELEASE.md section 1.5);
  whether the account needs the closed test (section 1.2); the Data safety form, the
  content rating, the target audience, the financial-features and cryptocurrency-policy
  declarations, and the listing and its screenshots (section 1.2).
- The device checks on an arm64 phone, and the emulator checks on the universal APK
  (RELEASE.md sections 6.4 and 8).
