//! Tawara for Android and iOS: the entry points the platforms start, and
//! the glue each needs (docs/DECISIONS.md D32).
//!
//! The same `tawara-app` runs here as on the desktop, through
//! [`tawara_app::run_in`] with a [`tawara_app::Host`]: the app-private
//! directory, the platform's clipboard, its share sheet and Android's Back.
//! What else each platform needs is done before the application starts or
//! from the platform's own notices:
//!
//! - **Android** (`src/android.rs`): `FLAG_SECURE` on the whole window (D32 item
//!   4); the store in `no_backup/` (D21), which the manifest's backup rules
//!   also exclude (platform/android); the lock when the system suspends the
//!   application, from iced's patched shell (D32 item 6); the QR scanner, whose
//!   camera is the NDK's Camera2 and whose frames are read by `src/qr.rs`
//!   (`src/android/scan.rs`, D42), stopped when the scan ends, when the
//!   application moves to the background, when its window loses the focus, and
//!   when the application ends.
//! - **iOS** (`src/ios.rs`): the store in Application Support, protected with
//!   `NSFileProtectionComplete` and excluded from backups (D32 item 3); a
//!   cover over the window whenever the application is not active, so the
//!   app switcher's snapshot shows nothing; the lock when it enters the
//!   background; the QR scanner, whose camera stops when the scan ends and when
//!   the application enters the background (`src/ios/scan.rs`, D42); and the
//!   screenshot and capture reports that the recovery phrase's screen reads
//!   (D44 item 3).
//!
//! Nothing here is Java, Kotlin, Swift or Objective-C (docs/DECISIONS.md
//! D4): the platform calls are made from Rust, through JNI on Android and
//! the Objective-C runtime on iOS.

#[cfg(target_os = "android")]
pub mod android;
#[cfg(target_os = "ios")]
pub mod ios;
// The QR reader behind Android's scanner. It is compiled for the tests on every
// host, so that the decoding is checked without a phone.
#[cfg(any(target_os = "android", test))]
mod qr;

/// The Android entry point: android-activity calls it on its own thread
/// once the activity is created. Its `AndroidApp` is the fork's winit's,
/// through `iced_winit`, the one copy of winit in the build (D32 item 9 (b)).
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: iced_winit::winit::platform::android::activity::AndroidApp) {
    android::main(app);
}
