//! Android: the window's flags, the store's directory, the clipboard, Back
//! and the lock, with no Java code (docs/DECISIONS.md D4, D32).
//!
//! The activity is `android.app.NativeActivity` (platform/android). JNI
//! reaches the platform's classes through the `Application` that
//! android-activity registers with `ndk-context`, as phase 1's spike did
//! (Tawara-wallet's docs/spikes/P1-REPORT.md). What this writes on stderr,
//! and the exit line written directly, goes to logcat under the tag
//! `RustStdoutStderr`, one `TAWARA` line per fact, never a secret.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

// winit is the fork's vendored copy, reached through iced_winit so that the
// build holds one copy of it (D32 item 9 (b)).
use iced_winit::winit::platform::android::activity::{AndroidApp, WindowManagerFlags};
use jni::objects::{JObject, JString, JValue};
use jni::vm::JavaVM;
use jni::{jni_sig, jni_str};
use tawara_app::{Host, Leaving};

/// The activity, for Back.
static APP: OnceLock<AndroidApp> = OnceLock::new();

/// The lock that the last move to the background started, kept for the line
/// the process writes as it ends (D32 item 9). `None` until the system has
/// suspended the application.
static LEAVING: Mutex<Option<Leaving>> = Mutex::new(None);

/// Start the application in the activity android-activity has created.
pub fn main(app: AndroidApp) {
    // No screenshot or recording of any screen, and no recent-apps
    // thumbnail (D32 item 4). Before the first frame, so none is taken.
    app.set_window_flags(WindowManagerFlags::SECURE, WindowManagerFlags::empty());
    let Some(private_dir) = private_dir(&app) else {
        eprintln!("TAWARA stop: the app-private directory could not be made");
        std::process::exit(1);
    };
    eprintln!("TAWARA start: private directory {}", private_dir.display());
    let _ = APP.set(app.clone());
    iced_winit::set_android_app(app);
    iced_winit::on_lifecycle(lifecycle);
    let host = Host {
        private_dir,
        copy,
        back,
    };
    if let Err(error) = tawara_app::run_in(host) {
        eprintln!("TAWARA stop: {error}");
    }
    // Asked without waiting, so the line shows whether the lock is finished
    // when the process ends (D32 item 9). A wait here would not finish it:
    // the interface takes the worker's answer only while the event loop runs,
    // and the loop has ended with the Activity (D32 item 9 (j)).
    log_exit(lock_state());
    // The process ends with this one, so the next launch starts cold (D32
    // item 9 (d)). The fork's winit allows a second event loop once this one
    // is dropped, but a second `android_main` here would need the Activity's
    // handles, the host statics and FLAG_SECURE built again, for a rare event.
    // The process ends with `_exit`, not `exit`, because `exit` runs the C
    // library's atexit handlers and the static destructors of the libraries
    // in the process while the window's render threads still run. The
    // emulator's exits aborted with SIGABRT in that state, bionic reporting a
    // lock on a destroyed mutex (D32 item 9 (j)). `_exit` runs none of them.
    // SAFETY: `_exit` takes no pointer and does not return.
    unsafe { libc::_exit(0) }
}

/// `no_backup/`, beside `files/`: the system leaves it out of Auto Backup
/// and device transfer, and the manifest's rules exclude the rest (D21,
/// D32 item 7). `files/` itself is mode `0771`, which the library refuses
/// for a store's parent (Tawara-wallet's docs/spikes/P1-REPORT.md, A19);
/// `no_backup/` is made `0700` when the system has not made it yet.
fn private_dir(app: &AndroidApp) -> Option<PathBuf> {
    let files = app.internal_data_path()?;
    let dir = files.parent()?.join("no_backup");
    make_private(&dir).then_some(dir)
}

fn make_private(dir: &Path) -> bool {
    use std::os::unix::fs::DirBuilderExt;
    dir.is_dir()
        || std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .is_ok()
}

/// The system suspends the application when it leaves the screen: its
/// window is destroyed. The wallet locks there and then (D32 item 6).
fn lifecycle(event: iced_winit::Lifecycle) {
    if event == iced_winit::Lifecycle::Suspended {
        eprintln!("TAWARA lifecycle: suspended; locking");
        // Not waited for: Android's process is not suspended the moment
        // this returns, and bounding the wait here as on iOS was declined
        // for now (D32 item 6). The Leaving is kept for the exit's line.
        let leaving = tawara_app::left_foreground();
        *LEAVING.lock().unwrap_or_else(PoisonError::into_inner) = Some(leaving);
    }
}

/// Whether the lock has finished, asked without waiting: `Leaving::wait`
/// with no time to wait answers at once, `true` once the interface has done
/// with the move, or when there was nothing to do (D32 item 9).
fn lock_state() -> &'static str {
    let kept = LEAVING.lock().unwrap_or_else(PoisonError::into_inner);
    match kept.as_ref() {
        None => "no lock was started",
        Some(leaving) if leaving.wait(Duration::ZERO) => "lock done",
        Some(_) => "lock not done",
    }
}

/// Logs the exit with the lock's state. It goes to logcat directly, not
/// through stderr: the thread that forwards stderr to logcat, in
/// android-activity (`forward_stdio_to_logcat`), may not have read a line
/// written just before `exit`, and then the line is lost. `Log.i` hands the
/// line to logd before it returns (D32 item 9).
fn log_exit(state: &str) {
    if let Err(error) = log_line(&format!("TAWARA exit: {state}")) {
        eprintln!("TAWARA exit: not logged: {error:?}");
    }
}

/// Puts `text` on the clipboard through `ClipboardManager`, marked sensitive
/// (docs/DECISIONS.md D32 item 9 (d) and item 10). From Android
/// 13 the system does not show a sensitive copy in its clipboard preview, and
/// earlier versions ignore the mark. A destination or a signed spend is not
/// secret, but a copy should not put it on the screen uninvited.
fn copy(text: &str) {
    if let Err(error) = clipboard_write(text) {
        eprintln!("TAWARA clipboard: {error:?}");
    }
}

/// Back: move the task to the background, as Android 12 and later do
/// themselves for a launcher's first activity, keeping the activity and
/// winit's event loop (Tawara-wallet's docs/spikes/P1-REPORT.md, A14 and
/// A18).
fn back() {
    let Some(app) = APP.get() else {
        return;
    };
    if let Err(error) = move_task_to_back(app) {
        eprintln!("TAWARA back: {error:?}");
    }
}

fn with_context<T>(
    f: impl FnOnce(&mut jni::Env, &JObject) -> jni::errors::Result<T>,
) -> jni::errors::Result<T> {
    let ctx = ndk_context::android_context();
    // SAFETY: android-activity initialised ndk-context with a valid JavaVM
    // and a global reference to the Application before android_main.
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) };
    let context = ctx.context() as jni::sys::jobject;
    vm.attach_current_thread(|env| {
        // SAFETY: a global reference that android-activity never deletes.
        let context = unsafe { env.as_cast_raw::<JObject>(&context)? };
        f(env, &context)
    })
}

/// Writes `line` to logcat under the tag the application's stderr lines carry.
fn log_line(line: &str) -> jni::errors::Result<()> {
    with_context(|env, _context| {
        let tag = env.new_string("RustStdoutStderr")?;
        let message = env.new_string(line)?;
        env.call_static_method(
            jni_str!("android/util/Log"),
            jni_str!("i"),
            jni_sig!((java.lang.String, java.lang.String) -> jint),
            &[JValue::Object(&tag), JValue::Object(&message)],
        )?;
        Ok(())
    })
}

fn clipboard_write(text: &str) -> jni::errors::Result<()> {
    with_context(|env, context| {
        let name = env.new_string("clipboard")?; // Context.CLIPBOARD_SERVICE
        let manager = env
            .call_method(
                context,
                jni_str!("getSystemService"),
                jni_sig!((java.lang.String) -> java.lang.Object),
                &[JValue::Object(&name)],
            )?
            .l()?;
        let label = env.new_string(tawara_app::DISPLAY_NAME)?;
        let value: JString = env.new_string(text)?;
        let clip = env
            .call_static_method(
                jni_str!("android/content/ClipData"),
                jni_str!("newPlainText"),
                jni_sig!((java.lang.CharSequence, java.lang.CharSequence) -> android.content.ClipData),
                &[JValue::Object(&label), JValue::Object(&value)],
            )?
            .l()?;
        // The mark goes on the clip's own description, which the manager
        // copies along with the clip, so it is set before setPrimaryClip.
        // setExtras is API 24 and the minimum here is 26. The key is
        // ClipDescription.EXTRA_IS_SENSITIVE, written out because reading
        // that field fails below API 33, and earlier versions ignore the key
        // (docs/DECISIONS.md D32 item 10). `with_context` catches any
        // exception these calls throw, so none is left pending. A failure
        // anywhere here returns before setPrimaryClip and leaves the
        // clipboard as it was, on purpose: every copy is meant to carry the
        // mark, so none goes on the clipboard without it.
        let description = env
            .call_method(
                &clip,
                jni_str!("getDescription"),
                jni_sig!(() -> android.content.ClipDescription),
                &[],
            )?
            .l()?;
        let extras = env.new_object(
            jni_str!("android/os/PersistableBundle"),
            jni_sig!(() -> void),
            &[],
        )?;
        let key = env.new_string("android.content.extra.IS_SENSITIVE")?;
        env.call_method(
            &extras,
            jni_str!("putBoolean"),
            jni_sig!((java.lang.String, jboolean) -> void),
            &[JValue::Object(&key), JValue::Bool(true)],
        )?;
        env.call_method(
            &description,
            jni_str!("setExtras"),
            jni_sig!((android.os.PersistableBundle) -> void),
            &[JValue::Object(&extras)],
        )?;
        env.call_method(
            &manager,
            jni_str!("setPrimaryClip"),
            jni_sig!((android.content.ClipData) -> void),
            &[JValue::Object(&clip)],
        )?;
        Ok(())
    })
}

fn move_task_to_back(app: &AndroidApp) -> jni::errors::Result<()> {
    // SAFETY: both pointers stay valid while `app` is alive (android-activity
    // documents `vm_as_ptr` and `activity_as_ptr` this way).
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let activity = app.activity_as_ptr() as jni::sys::jobject;
    vm.attach_current_thread(|env| {
        // SAFETY: the activity's own reference, alive while `app` is.
        let activity = unsafe { env.as_cast_raw::<JObject>(&activity)? };
        env.call_method(
            &activity,
            jni_str!("moveTaskToBack"),
            jni_sig!((jboolean) -> jboolean),
            &[JValue::Bool(true)],
        )?;
        Ok(())
    })
}
