# iOS platform glue

Tawara on iOS is `crates/mobile`'s `tawara` binary as the app bundle's
executable, with an `Info.plist` made from `Info.plist.in`: no Xcode
project, storyboard, Swift or Objective-C (docs/DECISIONS.md D4, D32).
Bundle identifier: `com.patricksmithlaravel.tawara` (D8).

- `Info.plist.in`: the bundle's keys, among them the scene manifest
  without which UIKit stops an app linked with the iOS 27 SDK at launch
  (D32 item 8), the status bar keys, which keep the clock and icons light on
  the dark screens (D27 item 8), and `NSFaceIDUsageDescription`, the usage
  text an app must carry to use Face ID, which the device unlock needs
  (D45 item 6; its wording is provisional until the owner approves it), and
  `NSCameraUsageDescription`, the usage text the QR scanner's camera needs
  (D42; provisional until the owner approves it);
  `tawara.sh` fills in the platform, the minimum system, the version and the
  build number, which is `CFBundleVersion` (`platform/build-number.sh`).
- `tawara.sh`: `device` (built and linked for a phone; installing it needs
  signing, phase 5's) and `simulator` (bundled, signed ad hoc, run on a
  simulator with the checks CI runs, `.github/workflows/mobile.yml`). Those
  checks do not exercise the device unlock: `simctl` has no biometric command,
  and Face ID is enrolled in the Simulator app's Features menu. Nor do they
  exercise the scanner: the simulator has no camera, so a scan there is refused
  at once (`TAWARA scan: no camera`).
- Frameworks: the device unlock links `Security` and `LocalAuthentication`
  itself, with `#[link]` in `crates/mobile/src/ios/keychain.rs`, and reaches
  them through `objc2` and `objc2-foundation` (whose `NSData` feature is new
  in `Cargo.toml`). The QR scanner links `AVFoundation` the same way, in
  `crates/mobile/src/ios/scan.rs`, with its bindings written by hand on
  `objc2` (D42). No framework is added to a project, since there is none.

What the Rust side does (`crates/mobile/src/ios.rs`): the store under
`Library/Application Support/Tawara`, made mode `0700`, protected with
`NSFileProtectionComplete` (D32 item 3) and excluded from iCloud and device
backups with `NSURLIsExcludedFromBackupKey` (docs/PLAN.md section 4.5); the
pasteboard, where a copy is local to the device and expires after two
minutes (D32 item 10); the share sheet, a `UIActivityViewController` without
its own Copy, on the top controller of the key window; a cover over the key
window whenever the application is not active, so the app switcher's snapshot
shows nothing (section 4.3), or, if UIKit has no key window, no cover and a log
line saying so, which fails the simulator check; the lock when it enters the
background. The screenshot and capture reports that the recovery phrase's
screen reads come from `UIApplicationUserDidTakeScreenshotNotification` and
`UIScreenCapturedDidChangeNotification` (D44 item 3). Putting winit's window in
the window scene UIKit connects is the iced fork's, since winit 0.30 has no
scene support of its own (D32 item 8); the app's part is the manifest.

The QR scanner (`crates/mobile/src/ios/scan.rs`; docs/DECISIONS.md D42) fills
the `Host` fields that start a scan and stop it, and the one that says the
shell draws the camera's preview. A scan is a full-screen modal over the key
window, presented as the share sheet is, with the preview and a Cancel button.
It is portrait only. The session starts and stops on a serial queue of its own,
and the readings come on another. The first reading that holds one distinct
text goes back to the application once, unchecked: the application trims it,
bounds it and checks it as a destination (D42 item 4). A reading of two
different texts gives nothing, and the camera reads on. Then the camera stops
and the scanner goes. A scan stops in the same way when the person cancels it,
when the application asks, and when the application goes to the background. The
camera is asked for only when a scan starts. The words the scanner shows, and
the usage text above, are provisional until the owner approves them (D42).
Android's scanner is its own, in `crates/mobile/src/android/scan.rs`, and is
described in platform/android/README.md.

The device unlock (`crates/mobile/src/ios/keychain.rs`; docs/DECISIONS.md D45)
fills the `Host` fields that offer the check and keep the store's password
for it. Android leaves them empty, since it offers no device unlock (D45
item 1). The check is the device's own, asked through `LAContext`: where the
device has no passcode no check is offered; otherwise the check is Face ID or
Touch ID where its biometry can be used, and the passcode where it cannot. The
password is one generic-password item in the Keychain, under the service
`com.patricksmithlaravel.tawara.unlock` and labelled with the store's
identifier: the 32 random bytes the application keeps in its folder, which are
not secret, written as lower-case hex. Its access control asks for the device's
check (`kSecAccessControlUserPresence`), and the item is readable only while the
device has a passcode, on this device only (D45 item 2). Keeping a password
replaces what is kept, and an item under the service whose identifier belongs
to another store is deleted, never used (D45 item 4). A release asks the check
on a thread of its own, with the reason the application passes, and answers
once. The copy made for the Keychain is wiped once the Keychain has taken its
own, and a password read back moves into the wallet's zeroizing buffer at once.
What this writes on stderr is `TAWARA unlock`, one line per fact, and no line
carries a password or its bytes. The owner approved the six choices of the
application side of the device unlock on 2026-10-17 (owner, 2026-10-17); the
wording of the Face ID usage text stays provisional until the owner approves
it (D45 item 6).

On the simulator the Keychain refuses an add from a binary signed ad hoc with
no entitlements, as `tawara.sh` signs the bundle: the add fails with -34018 (a
required entitlement is not present), and no password is kept. That was seen
for a test binary signed the same way; the bundle itself was not run through
the add. The device unlock can keep a password on a simulator only once a
signed build carries the entitlement it needs (phase 5's signing). `simctl`
has no command for Face ID, so Face ID is enrolled in the Simulator app's
Features menu, not from the command line.

Phase 5's signed build adds the `com.apple.developer.default-data-protection`
entitlement, so that every file the application makes is protected from
the start, not only those under the folder `tawara.sh` checks.

## The App Store upload

`release.sh` makes `Tawara-X.Y.Z+N.ipa`, as Tawara-wallet's `docs/RELEASE.md`
section 6.5 and `docs/DECISIONS.md` D36 describe. It runs on the owner's Mac, from
the commit that is tagged for the upload (RELEASE.md sections 5.1 to 5.4). CI never
runs it (D13, D36 item 4). D36 is proposed, and the script follows its
recommendations, which await the owner's decision.

It builds the `aarch64-apple-ios` release binary and makes `Tawara.app` from
`Info.plist.in`, with the `DT` keys of the Xcode in use, the build number as
`CFBundleVersion`, the icon compiled into `Assets.car` with `actool`,
`PrivacyInfo.xcprivacy`, and `THIRD-PARTY-NOTICES.txt` and `LICENSE.md` at the
bundle's root (RELEASE.md section 4). With the signing variables set, it writes the
entitlements, embeds the App Store profile, signs the bundle, reads the entitlements
back, zips `Payload` with `ditto` and checks the archive. It prints the `altool`
upload commands and runs none of them.

The environment. None of these values is written into a file the repository keeps.

- `TAWARA_EXPORT_COMPLIANCE`, required in every mode: `true` or `false`, counsel's
  answer on encryption (RELEASE.md section 1.2). It becomes
  `ITSAppUsesNonExemptEncryption`, which `Info.plist.in` leaves out on purpose. There
  is no default.
- `TAWARA_BUILD_NUMBER`, required for a release: the committed counter. Run
  `platform/build-number.sh bump N`, commit it, then set this to N. A dry run takes
  the development number, 1.
- `TAWARA_IOS_ICON_DIR`, required for a release: the folder that holds
  `Assets.xcassets`, with an `AppIcon` set. The icon is the owner's. A dry run
  without it leaves the icon out, with a warning.
- `TAWARA_TEAM_ID`, `TAWARA_IOS_PROFILE`, `TAWARA_IOS_SIGN_ID`: the team, the App
  Store profile and the Apple Distribution identity. If any is unset, the script
  stops before signing, names what is unset, makes no `.ipa` and prints no upload
  command. It exits 0 there, so read the last line.
- `TAWARA_IOS_DRY_RUN=1`: build and check, then stop before signing. A dry run is
  never uploaded, so an SDK other than 26.x is a warning in a dry run and a refusal
  in a release.
- `DEVELOPER_DIR`: when it is unset and `/Applications/Xcode_26.6.app` exists, that
  Xcode is used. Otherwise the active one is, and a release needs both the iPhoneOS
  SDK and the SDK the binary is linked with (`vtool -show-build`) to be 26.x.
- `TAWARA_IOS_OUT`: where the bundle and the `.ipa` go; `target/ios-release` by
  default, which git ignores.

Before anything is signed, a release refuses: a missing `TAWARA_BUILD_NUMBER` or
icon; tracked files that differ from the commit; a build number that is not the
committed counter (`bn_number`, step 1 above); a `tawara-mobile` version that differs
from the `tawara-app` version in `Cargo.lock` (RELEASE.md section 5.1); a binary that
is not an iOS device binary with minos 16.0; and a non-26.x SDK. The profile must be
an App Store profile for this app, with `get-task-allow` false and no device list.
After signing, the entitlements codesign reads back must equal the file the bundle
was signed with. The archive must hold `Payload/Tawara.app/` and no AppleDouble `._`
entry.

Entitlements (`write_entitlements`): `application-identifier` and
`com.apple.developer.team-identifier` from `TAWARA_TEAM_ID`;
`com.apple.developer.default-data-protection` as `NSFileProtectionComplete` (D32
item 3); `get-task-allow` false. There is no `keychain-access-groups`. The application
identifier is the default Keychain access group, and the item D45 keeps belongs to
this app alone. The -34018 failure above comes from a bundle with no application
identifier, and an ad hoc signed bundle has none.

Privacy manifest (`PrivacyInfo.xcprivacy`), from `nm -u` on the release binary:
`C617.1` for FileTimestamp, because the binary imports `stat`, `lstat`, `fstat` and
`fstatat`, which read the metadata of files, and the store's files are in the app's
container. That reading is taken from the code, and the owner's Xcode privacy report
on the archive confirms it. `35F9.1` for SystemBootTime, because the binary imports
`mach_absolute_time`, which Rust's `Instant` uses. The binary
imports no disk-space call, `NSUserDefaults` or active-keyboard API, so none is
declared. It also imports `clock_gettime`, and no reason code for it was found in the
sources checked, so the App Store validation of the 26.x archive decides it.
`NSPrivacyTracking` is false. `NSPrivacyCollectedDataTypes` is empty, on the reasoning
recorded in the file. Who operates the default node, and whether it keeps the
requests, is open (D43 item 5; the threat model's T28). The owner decides the App
Store privacy answers before submission, and they must agree with the file. The
imports were read from a binary linked with the iOS 27.0 SDK, the one on the machine
where they were checked, so they are read again from the 26.x build before
submission.

Not done here, for the owner: the Apple Developer enrolment, identifiers,
certificates, profile and API key (RELEASE.md section 1.2); the icon; the
`TAWARA_EXPORT_COMPLIANCE` answer; the App Store privacy answers; the upload; and the
checks on the signed package on a device (RELEASE.md sections 7 and 8).
