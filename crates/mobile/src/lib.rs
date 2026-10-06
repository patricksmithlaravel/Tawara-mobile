//! Tawara for Android and iOS: the entry points the platforms start, and
//! the glue each needs (docs/DECISIONS.md D32).
//!
//! The same `tawara-app` runs here as on the desktop, through
//! [`tawara_app::run_in`] with a [`tawara_app::Host`]: the app-private
//! directory, the platform's clipboard and Android's Back. What else each
//! platform needs is done before the application starts or from the
//! platform's own notices:
//!
//! - **Android** (`src/android.rs`): `FLAG_SECURE` on the whole window (D32 item
//!   4); the store in `no_backup/` (D21), which the manifest's backup rules
//!   also exclude (platform/android); the lock when the system suspends the
//!   application, from iced's patched shell (D32 item 6).
//! - **iOS** (`src/ios.rs`): the store in Application Support, protected with
//!   `NSFileProtectionComplete` and excluded from backups (D32 item 3); a
//!   cover over the window whenever the application is not active, so the
//!   app switcher's snapshot shows nothing; the lock when it enters the
//!   background.
//!
//! Nothing here is Java, Kotlin, Swift or Objective-C (docs/DECISIONS.md
//! D4): the platform calls are made from Rust, through JNI on Android and
//! the Objective-C runtime on iOS.

#[cfg(target_os = "android")]
pub mod android;
#[cfg(target_os = "ios")]
pub mod ios;

/// The Android entry point: android-activity calls it on its own thread
/// once the activity is created.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    android::main(app);
}
