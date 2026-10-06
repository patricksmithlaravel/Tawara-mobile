//! Android: the window's flags, the store's directory, the clipboard, Back
//! and the lock, with no Java code (docs/DECISIONS.md D4, D32).
//!
//! The activity is `android.app.NativeActivity` (platform/android). JNI
//! reaches the platform's classes through the `Application` that
//! android-activity registers with `ndk-context`, as phase 1's spike did
//! (docs/spikes/P1-REPORT.md). What this writes on stderr goes to logcat
//! under the tag `RustStdoutStderr`, one `TAWARA` line per fact, never a
//! secret.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use jni::objects::{JObject, JString, JValue};
use jni::vm::JavaVM;
use jni::{jni_sig, jni_str};
use tawara_app::Host;
use winit::platform::android::activity::{AndroidApp, WindowManagerFlags};

/// The activity, for Back.
static APP: OnceLock<AndroidApp> = OnceLock::new();

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
    // winit allows one event loop per process: a second `android_main` here
    // could not run one, so the process ends with this one.
    std::process::exit(0);
}

/// `no_backup/`, beside `files/`: the system leaves it out of Auto Backup
/// and device transfer, and the manifest's rules exclude the rest (D21,
/// D32 item 7). `files/` itself is mode `0771`, which the library refuses
/// for a store's parent (docs/spikes/P1-REPORT.md, A19); `no_backup/` is
/// made `0700` when the system has not made it yet.
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
        tawara_app::left_foreground();
    }
}

/// Puts `text` on the clipboard through `ClipboardManager`.
fn copy(text: &str) {
    if let Err(error) = clipboard_write(text) {
        eprintln!("TAWARA clipboard: {error:?}");
    }
}

/// Back: move the task to the background, as Android 12 and later do
/// themselves for a launcher's first activity, keeping the activity and
/// winit's event loop (docs/spikes/P1-REPORT.md, A14 and A18).
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
