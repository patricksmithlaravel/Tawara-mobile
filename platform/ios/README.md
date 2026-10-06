# iOS platform glue

Tawara on iOS is `crates/mobile`'s `tawara` binary as the app bundle's
executable, with an `Info.plist` made from `Info.plist.in`: no Xcode
project, storyboard, Swift or Objective-C (docs/DECISIONS.md D4, D32).
Bundle identifier: `com.patricksmithlaravel.tawara` (D8).

- `Info.plist.in`: the bundle's keys, among them the scene manifest
  without which UIKit stops an app linked with the iOS 27 SDK at launch
  (D32 item 8); `tawara.sh` fills in the platform, the minimum system and
  the version.
- `tawara.sh`: `device` (built and linked for a phone; installing it needs
  signing, phase 5's) and `simulator` (bundled, signed ad hoc, run on a
  simulator with the checks CI runs, `.github/workflows/mobile.yml`).

What the Rust side does (`crates/mobile/src/ios.rs`): the store under
`Library/Application Support/Tawara`, made mode `0700`, protected with
`NSFileProtectionComplete` (D32 item 3) and excluded from iCloud and device
backups with `NSURLIsExcludedFromBackupKey` (docs/PLAN.md section 4.5); the
pasteboard; a cover over the window whenever the application is not active,
so the app switcher's snapshot shows nothing (section 4.3); the lock
when it enters the background. Putting winit's window in the window scene
UIKit connects is the iced fork's, since winit 0.30 has no scene support of
its own (D32 item 8); the app's part is the manifest.

Phase 5's signed build adds the `com.apple.developer.default-data-protection`
entitlement, so that every file the application makes is protected from
the start, not only those under the folder `tawara.sh` checks.
