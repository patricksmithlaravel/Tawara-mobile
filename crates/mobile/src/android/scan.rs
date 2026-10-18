//! Android: the QR scanner, through Camera2 and AImageReader of the NDK, with the
//! camera permission asked and polled through JNI (docs/DECISIONS.md D42).
//!
//! `android.rs` gives the application this scanner through the `Host` fields
//! `scan`, `cancel_scan` and `scan_preview`. Android's preview is drawn by the
//! application (`ScanPreview::Application`), and this module gives it each
//! frame's preview through `tawara_app::host::preview_frame`, ten a second at
//! most, turned upright for the display's rotation at the time.
//!
//! One scan runs at a time, on a thread of its own. The thread waits for the
//! permission when it has none, opens the back camera, and reads each frame's
//! luminance (`qr.rs`). The first text read goes back once, through the `done`
//! the scan was given, and the camera's objects are released before it does
//! (D42 item 4). A scan also ends when it is cancelled, when the application
//! moves to the background (`background`, which `android.rs` calls from the
//! lifecycle hook), when the window loses the focus it had (a system dialog, the
//! notification shade or Recents), when the camera is lost, and when the
//! application ends (`shutdown`).
//!
//! A NativeActivity gets no `onRequestPermissionsResult` (D4), so the answer is
//! polled with `checkSelfPermission` (D42 item 3). The permission dialog takes
//! the window's focus and gives it back when it closes, and the answer is read
//! then. A refusal that shows no dialog (Android shows none once the person has
//! chosen not to be asked again) changes no focus, and is taken after
//! `NO_DIALOG`. A refusal is `None`, which the application drops.
//!
//! Threads: `start`, `cancel`, `background` and `shutdown` run on the thread that
//! calls them. The scan's thread polls and reads. The camera's callbacks run on
//! the NDK's threads: they read a frame's luminance, give its preview, and hand
//! the frame to the scan's thread through `Inbox`, which keeps only the latest
//! frame. `Camera`'s `Drop` releases every NDK object, on whichever thread ends
//! the scan.
//!
//! What this writes on stderr is one `TAWARA scan` line per fact, never a text
//! that was read.

use std::ffi::{CStr, CString, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::slice;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use jni::objects::{JObject, JObjectArray, JValue};
use jni::{jni_sig, jni_str};
use ndk_sys::{
    ACameraCaptureSession, ACameraCaptureSession_close, ACameraCaptureSession_setRepeatingRequest,
    ACameraCaptureSession_stateCallbacks, ACameraCaptureSession_stopRepeating, ACameraDevice,
    ACameraDevice_StateCallbacks, ACameraDevice_close, ACameraDevice_createCaptureRequest,
    ACameraDevice_createCaptureSession, ACameraDevice_request_template, ACameraIdList,
    ACameraManager, ACameraManager_create, ACameraManager_delete,
    ACameraManager_deleteCameraIdList, ACameraManager_getCameraCharacteristics,
    ACameraManager_getCameraIdList, ACameraManager_openCamera, ACameraMetadata,
    ACameraMetadata_const_entry, ACameraMetadata_const_entry__bindgen_ty_1, ACameraMetadata_free,
    ACameraMetadata_getConstEntry, ACameraOutputTarget, ACameraOutputTarget_create,
    ACameraOutputTarget_free, ACaptureRequest, ACaptureRequest_addTarget, ACaptureRequest_free,
    ACaptureSessionOutput, ACaptureSessionOutput_create, ACaptureSessionOutput_free,
    ACaptureSessionOutputContainer, ACaptureSessionOutputContainer_add,
    ACaptureSessionOutputContainer_create, ACaptureSessionOutputContainer_free, AIMAGE_FORMATS,
    AImage, AImage_delete, AImage_getHeight, AImage_getPlaneData, AImage_getPlanePixelStride,
    AImage_getPlaneRowStride, AImage_getWidth, AImageReader, AImageReader_ImageListener,
    AImageReader_acquireLatestImage, AImageReader_delete, AImageReader_getWindow, AImageReader_new,
    AImageReader_setImageListener, ANativeWindow, acamera_metadata_enum_acamera_lens_facing,
    acamera_metadata_enum_acamera_scaler_available_stream_configurations, acamera_metadata_tag,
    camera_status_t, media_status_t,
};
use tawara_app::host::{ScanDone, preview_frame};

use super::with_activity;
use crate::qr;

// The camera's functions are in libcamera2ndk. ndk-sys declares them and links
// nothing for them (docs/DECISIONS.md D42 item 3). `mediandk`, which holds the
// image reader, is linked by ndk-sys with its `media` feature.
#[link(name = "camera2ndk")]
unsafe extern "C" {}

/// The camera permission, as Android names it (platform/android/AndroidManifest.xml).
const CAMERA: &str = "android.permission.CAMERA";
/// `PackageManager.PERMISSION_GRANTED`, which `checkSelfPermission` answers.
const GRANTED: i32 = 0;
/// The request code of the permission request. Nothing matches an answer to it,
/// since the answer is polled, so it is only a label.
const REQUEST_CODE: i32 = 42;
/// `AIMAGE_FORMAT_YUV_420_888`: the format of the frames, whose first plane is
/// the luminance.
const YUV_420_888: i32 = AIMAGE_FORMATS::AIMAGE_FORMAT_YUV_420_888.0 as i32;
/// `ACAMERA_SCALER_AVAILABLE_STREAM_CONFIGURATIONS_OUTPUT`: a configuration that
/// is an output, and not an input.
const OUTPUT: i32 =
    acamera_metadata_enum_acamera_scaler_available_stream_configurations::ACAMERA_SCALER_AVAILABLE_STREAM_CONFIGURATIONS_OUTPUT.0 as i32;
/// `ACAMERA_LENS_FACING_BACK`: the value of a camera that faces the back.
const LENS_FACING_BACK: u32 = acamera_metadata_enum_acamera_lens_facing::ACAMERA_LENS_FACING_BACK.0;
/// The frames the image reader holds at once. Each is taken as it arrives, so
/// two are enough for one being read and one arriving.
const FRAMES_HELD: i32 = 2;
/// The shortest time between two previews given to the application: ten a second.
const PREVIEW_EVERY: Duration = Duration::from_millis(100);
/// How long a thread waits for a frame, or for the permission, before it looks
/// again.
const TICK: Duration = Duration::from_millis(200);
/// How often a scan checks that its window still has the focus it had.
const FOCUS_EVERY: Duration = Duration::from_millis(250);
/// How long a request may go without the window losing the focus before it is
/// taken as having shown no dialog: the person has chosen not to be asked again,
/// or the system has refused the request.
const NO_DIALOG: Duration = Duration::from_secs(2);
/// How long a grant is given to be recorded after the dialog has gone, before
/// the answer is read.
const SETTLE: Duration = Duration::from_millis(300);
/// The longest a scan waits for the permission, in all.
const ASKING_LIMIT: Duration = Duration::from_secs(300);

/// The scan under way, from its start until its reply.
struct Scan {
    /// The number of the scan, so that a thread or a callback that ends an
    /// earlier scan does not end this one.
    number: u64,
    /// Where the text goes, once.
    done: ScanDone,
    /// The camera, once it is open. A scan that is asking for the permission, or
    /// still opening the camera, has none.
    camera: Option<Camera>,
    /// Shared with the camera's callbacks and with the scan's thread.
    inbox: Arc<Inbox>,
}

/// The scan under way, if there is one. Its lock is held only to read or change
/// it, and never across a call to the camera or to a reply.
static SCAN: Mutex<Option<Scan>> = Mutex::new(None);

/// The number of the last scan begun.
static SCANS: AtomicU64 = AtomicU64::new(0);

/// What `Host::scan` is: starts a scan, and gives back its text through `done`,
/// once. It returns at once. `Err` says why the scan did not start, in words for
/// the person (docs/DECISIONS.md D42).
pub(super) fn start(done: ScanDone) -> Result<(), String> {
    // The camera is looked for first, since that reads no state of the
    // interface's: a device with no back camera is refused whichever thread asks.
    let manager = Manager::new().ok_or_else(|| refuse("the camera service is not available"))?;
    let choice = back_camera(&manager).map_err(|why| refuse(&why))?;
    drop(manager);
    let number = SCANS.fetch_add(1, Ordering::SeqCst) + 1;
    let inbox = Arc::new(Inbox::new(choice.sensor));
    {
        let mut slot = scan_slot();
        if slot.is_some() {
            return Err(refuse("a scan is already under way"));
        }
        *slot = Some(Scan {
            number,
            done,
            camera: None,
            inbox: Arc::clone(&inbox),
        });
    }
    let Some(granted) = permission_granted() else {
        forget(number);
        return Err(refuse("the camera permission could not be read"));
    };
    if !granted && let Err(why) = request_permission() {
        forget(number);
        return Err(refuse(&why));
    }
    let spawned = thread::Builder::new()
        .name("tawara-scan".to_owned())
        .spawn(move || run(number, !granted, choice, inbox));
    if let Err(error) = spawned {
        forget(number);
        return Err(refuse(&format!(
            "the scanner's thread could not start: {error}"
        )));
    }
    eprintln!("TAWARA scan: started");
    Ok(())
}

/// What `Host::cancel_scan` is: ends the scan under way, if there is one, with no
/// text. Any thread; with no scan under way it does nothing.
pub(super) fn cancel() {
    stop("cancelled");
}

/// Ends the scan under way for a move to the background. The application's own
/// stop follows through `cancel`, but this runs first, on the event loop's
/// thread, so that no camera runs in the background (docs/DECISIONS.md D42
/// item 4).
pub(super) fn background() {
    stop("moved to the background");
}

/// Ends the scan under way when the application ends, so that the camera's
/// objects are released before the process ends (docs/DECISIONS.md D42 item 4).
pub(super) fn shutdown() {
    stop("the application ended");
}

/// Ends the scan under way, if there is one, with no text.
fn stop(why: &str) {
    let number = scan_slot().as_ref().map(|scan| scan.number);
    if let Some(number) = number {
        end(number, None, why);
    }
}

/// Ends scan `number` if it is still the one under way: its camera is released,
/// and its reply goes back once, with `reading`. Returns whether it ended the
/// scan. Any thread, and never while holding the scan's lock.
fn end(number: u64, reading: Option<Vec<u8>>, why: &str) -> bool {
    let taken = {
        let mut slot = scan_slot();
        if slot.as_ref().is_some_and(|scan| scan.number == number) {
            slot.take()
        } else {
            None
        }
    };
    let Some(Scan {
        done,
        camera,
        inbox,
        ..
    }) = taken
    else {
        return false;
    };
    inbox.end();
    // The camera is released before the reply goes back, so that the
    // application never shows a page for a camera that is still running.
    drop(camera);
    eprintln!("TAWARA scan: ended, {why}");
    done(reading);
    true
}

/// Forgets scan `number`, if it is still the one under way, without a reply: its
/// start was refused, and `start` says so to the application.
fn forget(number: u64) {
    let taken = {
        let mut slot = scan_slot();
        if slot.as_ref().is_some_and(|scan| scan.number == number) {
            slot.take()
        } else {
            None
        }
    };
    drop(taken);
}

/// The scan under way, locked for a read or a change only.
fn scan_slot() -> MutexGuard<'static, Option<Scan>> {
    SCAN.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Logs a refusal, and returns it for the application.
fn refuse(why: &str) -> String {
    eprintln!("TAWARA scan: {why}");
    why.to_owned()
}

/// The scan's thread: waits for the permission if the scan asked for it, opens
/// the camera, reads frames until a code is read or the scan ends, and then ends
/// the scan with what it read.
fn run(number: u64, asking: bool, choice: Choice, inbox: Arc<Inbox>) {
    if asking {
        match wait_for_permission(&inbox) {
            Permission::Granted => {}
            Permission::Refused => {
                end(number, None, "the camera access was refused");
                return;
            }
            Permission::Over => return,
        }
    }
    let camera = match open(&inbox, &choice) {
        Ok(camera) => camera,
        Err(why) => {
            end(number, None, &why);
            return;
        }
    };
    if let Some(camera) = attach(number, camera) {
        // The scan ended while the camera was opening, so the camera is released
        // here.
        drop(camera);
        return;
    }
    let (reading, why) = read_frames(&inbox);
    end(number, reading, why);
}

/// Gives `camera` to scan `number`, if that scan is still under way. Otherwise
/// the camera is handed back, to be released.
fn attach(number: u64, camera: Camera) -> Option<Camera> {
    let mut slot = scan_slot();
    match slot.as_mut().filter(|scan| scan.number == number) {
        Some(scan) => {
            scan.camera = Some(camera);
            None
        }
        None => Some(camera),
    }
}

/// Opens the back camera `choice` names, and starts its preview.
fn open(inbox: &Arc<Inbox>, choice: &Choice) -> Result<Camera, String> {
    let manager = Manager::new().ok_or_else(|| "the camera service is not available".to_owned())?;
    let mut camera = Camera::new(manager);
    camera.setup(inbox, choice)?;
    Ok(camera)
}

/// What the scan's thread found about the permission.
enum Permission {
    /// The camera may be used.
    Granted,
    /// The person refused it, or the system did.
    Refused,
    /// The scan ended while it waited.
    Over,
}

/// Waits for the answer to the permission request, polling it. A dialog takes the
/// window's focus when it appears and gives it back when it closes: the answer is
/// read after that, and a refusal with no dialog is taken after `NO_DIALOG`.
fn wait_for_permission(inbox: &Inbox) -> Permission {
    let asked = Instant::now();
    let mut unfocused = false;
    loop {
        if inbox.pause(TICK) {
            return Permission::Over;
        }
        if permission_granted() == Some(true) {
            return Permission::Granted;
        }
        match window_focused() {
            Some(false) => unfocused = true,
            Some(true) if unfocused => {
                // The dialog has gone and the window has its focus back. A moment
                // is given for the answer to be recorded.
                if inbox.pause(SETTLE) {
                    return Permission::Over;
                }
                return if permission_granted() == Some(true) {
                    Permission::Granted
                } else {
                    Permission::Refused
                };
            }
            _ => {}
        }
        if !unfocused && asked.elapsed() >= NO_DIALOG {
            return Permission::Refused;
        }
        if asked.elapsed() >= ASKING_LIMIT {
            return Permission::Refused;
        }
    }
}

/// Reads frames until one holds a code, which it returns with `"a code was read"`,
/// or until the scan ends or the window loses the focus it had, which is why it
/// stops with no text.
fn read_frames(inbox: &Inbox) -> (Option<Vec<u8>>, &'static str) {
    let mut focused = false;
    let mut checked = Instant::now();
    loop {
        match inbox.next(TICK) {
            Next::Frame(frame) => {
                if let Some(text) = qr::read(frame.width, frame.height, &frame.gray) {
                    return (Some(text), "a code was read");
                }
            }
            Next::Idle => {}
            Next::Over => return (None, "the camera stopped"),
        }
        if checked.elapsed() >= FOCUS_EVERY {
            checked = Instant::now();
            match window_focused() {
                Some(true) => focused = true,
                Some(false) if focused => return (None, "the window lost the focus"),
                _ => {}
            }
        }
    }
}

/// What the scan's thread found in the inbox.
enum Next {
    /// A frame, not read yet.
    Frame(Frame),
    /// Nothing came within the wait.
    Idle,
    /// The scan has ended, or the camera has been lost.
    Over,
}

/// What the camera's callbacks and the scan's thread share: the latest frame not
/// yet read, and whether the scan is over.
struct Inbox {
    state: Mutex<State>,
    /// Rung when a frame arrives, and when the scan ends or the camera is lost.
    moved: Condvar,
    /// The sensor's orientation, which turns the preview upright (`qr::turns`).
    sensor: i32,
}

#[derive(Default)]
struct State {
    /// The latest frame, until the scan's thread takes it. A newer one replaces it.
    frame: Option<Frame>,
    /// When the last preview was given to the application.
    previewed: Option<Instant>,
    /// The scan has ended: its thread leaves, and no frame is kept.
    over: bool,
    /// The camera reported an error, or was disconnected.
    lost: bool,
}

/// One frame's luminance: `width` by `height` samples, row by row from the top.
struct Frame {
    width: usize,
    height: usize,
    gray: Vec<u8>,
}

impl Inbox {
    fn new(sensor: i32) -> Inbox {
        Inbox {
            state: Mutex::new(State::default()),
            moved: Condvar::new(),
            sensor,
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the scan has ended, so that no frame is wanted.
    fn is_over(&self) -> bool {
        self.state().over
    }

    /// Whether a preview is due at `now`. If one is, it is taken as given now.
    fn preview_due(&self, now: Instant) -> bool {
        let mut state = self.state();
        let due = !state.over
            && state
                .previewed
                .is_none_or(|last| now.duration_since(last) >= PREVIEW_EVERY);
        if due {
            state.previewed = Some(now);
        }
        due
    }

    /// Keeps `frame` for the scan's thread, in place of one it has not taken.
    /// Nothing is kept once the scan is over.
    fn put(&self, frame: Frame) {
        let mut state = self.state();
        if !state.over {
            state.frame = Some(frame);
            self.moved.notify_all();
        }
    }

    /// The next thing for the scan's thread: a frame, or nothing within `within`,
    /// or that the scan is over.
    fn next(&self, within: Duration) -> Next {
        let (mut state, _) = self
            .moved
            .wait_timeout_while(self.state(), within, |state| {
                state.frame.is_none() && !state.over && !state.lost
            })
            .unwrap_or_else(PoisonError::into_inner);
        if state.over || state.lost {
            return Next::Over;
        }
        state.frame.take().map_or(Next::Idle, Next::Frame)
    }

    /// Waits up to `within`, and says whether the scan is over or the camera lost
    /// by then.
    fn pause(&self, within: Duration) -> bool {
        let (state, _) = self
            .moved
            .wait_timeout_while(self.state(), within, |state| !state.over && !state.lost)
            .unwrap_or_else(PoisonError::into_inner);
        state.over || state.lost
    }

    /// The scan is over: no frame is kept, and the thread that waits leaves.
    fn end(&self) {
        self.state().over = true;
        self.moved.notify_all();
    }

    /// The camera reported an error or was disconnected.
    fn lost(&self) {
        self.state().lost = true;
        self.moved.notify_all();
    }
}

/// An `ACameraManager`, deleted when it is dropped.
struct Manager(*mut ACameraManager);

impl Manager {
    fn new() -> Option<Manager> {
        // SAFETY: the call takes no arguments; a null result is refused below.
        let manager = unsafe { ACameraManager_create() };
        (!manager.is_null()).then_some(Manager(manager))
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        // SAFETY: the manager was made by `ACameraManager_create` and is deleted
        // once, here.
        unsafe { ACameraManager_delete(self.0) };
    }
}

/// A camera that is open for a scan, and the NDK objects it holds. `Drop`
/// releases them in the order the NDK needs: the session and the device first,
/// so that no frame is written once the reader it feeds is deleted, and the
/// reader before the context its callbacks read. A null handle was never made,
/// and is not released. The manager is released last, as the field below it.
struct Camera {
    manager: Manager,
    device: *mut ACameraDevice,
    session: *mut ACameraCaptureSession,
    request: *mut ACaptureRequest,
    target: *mut ACameraOutputTarget,
    container: *mut ACaptureSessionOutputContainer,
    output: *mut ACaptureSessionOutput,
    reader: *mut AImageReader,
    /// The `Inbox` the callbacks write to, from `Arc::into_raw`. Its count is given
    /// back once the reader is deleted.
    inbox: *const Inbox,
    /// The callbacks' structs. They live on the heap, since the NDK may keep their
    /// addresses, and they are kept until the camera is released.
    listener: Option<Box<AImageReader_ImageListener>>,
    device_callbacks: Option<Box<ACameraDevice_StateCallbacks>>,
    session_callbacks: Option<Box<ACameraCaptureSession_stateCallbacks>>,
}

// SAFETY: the NDK's camera and reader objects are not tied to the thread that
// made them. A camera is used by the scan's thread, and dropped by whichever
// thread ends the scan, and the raw pointers never leave this module.
unsafe impl Send for Camera {}

impl Camera {
    fn new(manager: Manager) -> Camera {
        Camera {
            manager,
            device: ptr::null_mut(),
            session: ptr::null_mut(),
            request: ptr::null_mut(),
            target: ptr::null_mut(),
            container: ptr::null_mut(),
            output: ptr::null_mut(),
            reader: ptr::null_mut(),
            inbox: ptr::null(),
            listener: None,
            device_callbacks: None,
            session_callbacks: None,
        }
    }

    /// Opens the camera `choice` names and starts its repeating preview request,
    /// with its frames read into `inbox`. `Err` says which step failed. What was
    /// made before it stays in `self`, so that `Drop` releases it.
    fn setup(&mut self, inbox: &Arc<Inbox>, choice: &Choice) -> Result<(), String> {
        self.inbox = Arc::into_raw(Arc::clone(inbox));
        let context = self.inbox.cast_mut().cast::<c_void>();

        let mut reader: *mut AImageReader = ptr::null_mut();
        // SAFETY: `reader` is a valid out pointer.
        let status = unsafe {
            AImageReader_new(
                choice.width,
                choice.height,
                YUV_420_888,
                FRAMES_HELD,
                &mut reader,
            )
        };
        media_ok(status, "the image reader")?;
        self.reader = reader;

        self.listener = Some(Box::new(AImageReader_ImageListener {
            context,
            onImageAvailable: Some(image_available),
        }));
        let listener = self
            .listener
            .as_deref_mut()
            .map_or(ptr::null_mut(), ptr::from_mut);
        // SAFETY: the reader is live, and the listener is on the heap, kept by this
        // camera until the reader is deleted.
        let status = unsafe { AImageReader_setImageListener(self.reader, listener) };
        media_ok(status, "the image listener")?;

        let mut window: *mut ANativeWindow = ptr::null_mut();
        // SAFETY: the reader is live and `window` is a valid out pointer. The
        // window belongs to the reader, which releases it, so it is not released
        // here (the NDK's header says so).
        let status = unsafe { AImageReader_getWindow(self.reader, &mut window) };
        media_ok(status, "the image window")?;

        let mut output: *mut ACaptureSessionOutput = ptr::null_mut();
        // SAFETY: the window is the reader's, and `output` is a valid out pointer.
        let status = unsafe { ACaptureSessionOutput_create(window, &mut output) };
        camera_ok(status, "the session output")?;
        self.output = output;

        let mut container: *mut ACaptureSessionOutputContainer = ptr::null_mut();
        // SAFETY: `container` is a valid out pointer.
        let status = unsafe { ACaptureSessionOutputContainer_create(&mut container) };
        camera_ok(status, "the session outputs")?;
        self.container = container;
        // SAFETY: the container and the output were made above and are live.
        let status = unsafe { ACaptureSessionOutputContainer_add(self.container, self.output) };
        camera_ok(status, "the session outputs")?;

        self.device_callbacks = Some(Box::new(ACameraDevice_StateCallbacks {
            context,
            onDisconnected: Some(device_lost),
            onError: Some(device_error),
        }));
        let callbacks = self
            .device_callbacks
            .as_deref_mut()
            .map_or(ptr::null_mut(), ptr::from_mut);
        let mut device: *mut ACameraDevice = ptr::null_mut();
        // SAFETY: the manager is live, `choice.id` is a name the NDK gave and ends
        // in NUL, the callbacks are on the heap until the device is closed, and
        // `device` is a valid out pointer.
        let status = unsafe {
            ACameraManager_openCamera(self.manager.0, choice.id.as_ptr(), callbacks, &mut device)
        };
        if status == camera_status_t::ACAMERA_ERROR_PERMISSION_DENIED {
            return Err("the camera is not allowed".to_owned());
        }
        camera_ok(status, "the camera")?;
        self.device = device;

        // The NDK refuses a session without callbacks, so an empty set is given.
        self.session_callbacks = Some(Box::new(ACameraCaptureSession_stateCallbacks {
            context,
            onClosed: None,
            onReady: None,
            onActive: None,
        }));
        let callbacks = self
            .session_callbacks
            .as_deref()
            .map_or(ptr::null(), ptr::from_ref);
        let mut session: *mut ACameraCaptureSession = ptr::null_mut();
        // SAFETY: the device and the container are live, the callbacks are on the
        // heap, and `session` is a valid out pointer.
        let status = unsafe {
            ACameraDevice_createCaptureSession(self.device, self.container, callbacks, &mut session)
        };
        camera_ok(status, "the capture session")?;
        self.session = session;

        let mut request: *mut ACaptureRequest = ptr::null_mut();
        // SAFETY: the device is live and `request` is a valid out pointer.
        let status = unsafe {
            ACameraDevice_createCaptureRequest(
                self.device,
                ACameraDevice_request_template::TEMPLATE_PREVIEW,
                &mut request,
            )
        };
        camera_ok(status, "the preview request")?;
        self.request = request;

        let mut target: *mut ACameraOutputTarget = ptr::null_mut();
        // SAFETY: the window is the reader's, and `target` is a valid out pointer.
        let status = unsafe { ACameraOutputTarget_create(window, &mut target) };
        camera_ok(status, "the preview target")?;
        self.target = target;
        // SAFETY: the request and the target were made above and are live.
        let status = unsafe { ACaptureRequest_addTarget(self.request, self.target) };
        camera_ok(status, "the preview target")?;

        let mut sequence: c_int = 0;
        // SAFETY: the session and the request are live, and one request is given.
        // No capture callbacks are given, which the NDK allows.
        let status = unsafe {
            ACameraCaptureSession_setRepeatingRequest(
                self.session,
                ptr::null_mut(),
                1,
                &mut self.request,
                &mut sequence,
            )
        };
        camera_ok(status, "the preview")?;
        Ok(())
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        // SAFETY: each handle is one this camera made and has not released, and a
        // null one was never made. The callbacks cannot run once the device is
        // closed, and the reader is deleted after that, so no frame reaches the
        // inbox after its count is given back.
        unsafe {
            if !self.session.is_null() {
                let _ = ACameraCaptureSession_stopRepeating(self.session);
                ACameraCaptureSession_close(self.session);
            }
            if !self.device.is_null() {
                ACameraDevice_close(self.device);
            }
            if !self.request.is_null() {
                ACaptureRequest_free(self.request);
            }
            if !self.target.is_null() {
                ACameraOutputTarget_free(self.target);
            }
            if !self.container.is_null() {
                ACaptureSessionOutputContainer_free(self.container);
            }
            if !self.output.is_null() {
                ACaptureSessionOutput_free(self.output);
            }
            if !self.reader.is_null() {
                AImageReader_delete(self.reader);
            }
            if !self.inbox.is_null() {
                drop(Arc::from_raw(self.inbox));
            }
        }
    }
}

/// The image callback: takes the latest image, copies its luminance, gives the
/// preview when one is due, and releases the image. It runs on the NDK's thread.
///
/// # Safety
///
/// `context` is the `Inbox` the listener was set with, and `reader` the reader
/// that called. Both are the NDK's to call with, as `AImageReader` documents.
unsafe extern "C" fn image_available(context: *mut c_void, reader: *mut AImageReader) {
    // SAFETY: the camera keeps the `Inbox` until its reader is deleted, and the NDK
    // calls the listener only while the reader exists.
    let inbox = unsafe { &*context.cast::<Inbox>() };
    // A panic must not cross into the NDK, which would end the process.
    let _ = catch_unwind(AssertUnwindSafe(|| read_image(inbox, reader)));
}

fn read_image(inbox: &Inbox, reader: *mut AImageReader) {
    if inbox.is_over() {
        return;
    }
    let mut image: *mut AImage = ptr::null_mut();
    // SAFETY: `reader` is the reader the callback named, and `image` a valid out
    // pointer.
    let status = unsafe { AImageReader_acquireLatestImage(reader, &mut image) };
    if status != media_status_t::AMEDIA_OK || image.is_null() {
        return;
    }
    let frame = luminance_of(image);
    // SAFETY: `image` was acquired above, and is released once, after its plane
    // has been copied.
    unsafe { AImage_delete(image) };
    let Some(frame) = frame else {
        return;
    };
    if inbox.preview_due(Instant::now()) {
        let turns = qr::turns(inbox.sensor, display_degrees());
        if let Some((width, height, rgba)) =
            qr::preview(frame.width, frame.height, &frame.gray, turns)
        {
            preview_frame(width, height, rgba);
        }
    }
    inbox.put(frame);
}

/// The luminance of an image's first plane, which is YUV's Y plane, copied out.
/// `None` when the plane cannot be read.
fn luminance_of(image: *mut AImage) -> Option<Frame> {
    let (mut width, mut height, mut row, mut pixel) = (0_i32, 0_i32, 0_i32, 0_i32);
    let mut data: *mut u8 = ptr::null_mut();
    let mut len: c_int = 0;
    // SAFETY: `image` is live, and each out pointer is valid for its type.
    let read = unsafe {
        AImage_getWidth(image, &mut width) == media_status_t::AMEDIA_OK
            && AImage_getHeight(image, &mut height) == media_status_t::AMEDIA_OK
            && AImage_getPlaneRowStride(image, 0, &mut row) == media_status_t::AMEDIA_OK
            && AImage_getPlanePixelStride(image, 0, &mut pixel) == media_status_t::AMEDIA_OK
            && AImage_getPlaneData(image, 0, &mut data, &mut len) == media_status_t::AMEDIA_OK
    };
    if !read || data.is_null() {
        return None;
    }
    let width = usize::try_from(width).ok()?;
    let height = usize::try_from(height).ok()?;
    let row = usize::try_from(row).ok()?;
    let pixel = usize::try_from(pixel).ok()?;
    let len = usize::try_from(len).ok()?;
    // SAFETY: the plane's `len` bytes are valid until the image is deleted, which
    // is after this copy.
    let plane = unsafe { slice::from_raw_parts(data, len) };
    let gray = qr::luminance(plane, width, height, row, pixel)?;
    Some(Frame {
        width,
        height,
        gray,
    })
}

/// The camera was disconnected: the scan ends with no text.
///
/// # Safety
///
/// `context` is the `Inbox` the device was opened with, which the camera keeps
/// until the device is closed.
unsafe extern "C" fn device_lost(context: *mut c_void, _device: *mut ACameraDevice) {
    // SAFETY: see the function's contract.
    unsafe { &*context.cast::<Inbox>() }.lost();
}

/// The camera reported an error: the scan ends with no text.
///
/// # Safety
///
/// As for `device_lost`.
unsafe extern "C" fn device_error(context: *mut c_void, _device: *mut ACameraDevice, code: c_int) {
    eprintln!("TAWARA scan: the camera reported error {code}");
    // SAFETY: see the function's contract.
    unsafe { &*context.cast::<Inbox>() }.lost();
}

/// The back camera of the device, and the size its frames are read at.
struct Choice {
    /// The camera's name, as Camera2 gives it.
    id: CString,
    /// `ACAMERA_SENSOR_ORIENTATION`: how many degrees clockwise the sensor's
    /// frames are from upright.
    sensor: i32,
    width: i32,
    height: i32,
}

/// The back camera of the device, with the size its frames are read at. `Err`
/// when the device has none.
fn back_camera(manager: &Manager) -> Result<Choice, String> {
    let names = camera_names(manager)?;
    names
        .iter()
        .find_map(|name| back_choice(manager, name))
        .ok_or_else(|| "this device has no back camera".to_owned())
}

/// The names of the device's cameras. The NDK's list is released before this
/// returns, on every path.
fn camera_names(manager: &Manager) -> Result<Vec<CString>, String> {
    let mut list: *mut ACameraIdList = ptr::null_mut();
    // SAFETY: the manager is live, and `list` is a valid out pointer.
    let status = unsafe { ACameraManager_getCameraIdList(manager.0, &mut list) };
    camera_ok(status, "the camera list")?;
    if list.is_null() {
        return Ok(Vec::new());
    }
    // SAFETY: `list` is the NDK's list, live until it is deleted just below.
    let names = unsafe { read_names(list) };
    // SAFETY: the list was made by `ACameraManager_getCameraIdList` and is deleted
    // once. Its names were copied above.
    unsafe { ACameraManager_deleteCameraIdList(list) };
    Ok(names)
}

/// Copies the names out of a list the NDK made.
///
/// # Safety
///
/// `list` is live: a list from `ACameraManager_getCameraIdList` not yet deleted.
unsafe fn read_names(list: *const ACameraIdList) -> Vec<CString> {
    // SAFETY: see the function's contract. Each name is NUL-terminated and valid
    // until the list is deleted.
    unsafe {
        let count = usize::try_from((*list).numCameras).unwrap_or(0);
        (0..count)
            .map(|index| CStr::from_ptr(*(*list).cameraIds.add(index)).to_owned())
            .collect()
    }
}

/// The choice for camera `name`, if it faces the back and offers a YUV size to
/// read its frames at.
fn back_choice(manager: &Manager, name: &CString) -> Option<Choice> {
    let mut meta: *mut ACameraMetadata = ptr::null_mut();
    // SAFETY: the manager is live, `name` ends in NUL, and `meta` is a valid out
    // pointer.
    let status =
        unsafe { ACameraManager_getCameraCharacteristics(manager.0, name.as_ptr(), &mut meta) };
    if status != camera_status_t::ACAMERA_OK || meta.is_null() {
        return None;
    }
    let choice = read_choice(meta, name);
    // SAFETY: the characteristics were made above and are released once, here.
    // Nothing read from them is kept.
    unsafe { ACameraMetadata_free(meta) };
    choice
}

/// The choice for the camera whose characteristics are `meta`, if it faces the
/// back and offers a YUV size to read its frames at.
fn read_choice(meta: *const ACameraMetadata, name: &CString) -> Option<Choice> {
    let facing = entry(meta, acamera_metadata_tag::ACAMERA_LENS_FACING)?;
    // SAFETY: the lens facing is one byte, and an entry the camera gives has at
    // least one value.
    let facing = unsafe { *facing.data.u8_ };
    if u32::from(facing) != LENS_FACING_BACK {
        return None;
    }
    let sensor = entry(meta, acamera_metadata_tag::ACAMERA_SENSOR_ORIENTATION)
        // SAFETY: the sensor orientation is an int32, with at least one value.
        .map_or(0, |entry| unsafe { *entry.data.i32_ });
    let (width, height) = qr::frame_size(&yuv_sizes(meta)?)?;
    Some(Choice {
        id: name.clone(),
        sensor,
        width,
        height,
    })
}

/// The output sizes the camera offers for `YUV_420_888`, as width and height.
fn yuv_sizes(meta: *const ACameraMetadata) -> Option<Vec<(i32, i32)>> {
    let configurations = entry(
        meta,
        acamera_metadata_tag::ACAMERA_SCALER_AVAILABLE_STREAM_CONFIGURATIONS,
    )?;
    let count = usize::try_from(configurations.count).ok()?;
    // SAFETY: the entry holds `count` int32 values, four to a configuration: the
    // format, the width, the height, and whether it is an input. It is valid while
    // the characteristics are.
    let values = unsafe { slice::from_raw_parts(configurations.data.i32_, count) };
    let (values, _) = values.as_chunks::<4>();
    Some(
        values
            .iter()
            .filter(|configuration| configuration[0] == YUV_420_888 && configuration[3] == OUTPUT)
            .map(|configuration| (configuration[1], configuration[2]))
            .collect(),
    )
}

/// The entry the camera gives for `tag`, if it gives one with at least one value.
/// Its data is valid while `meta` is.
fn entry(
    meta: *const ACameraMetadata,
    tag: acamera_metadata_tag,
) -> Option<ACameraMetadata_const_entry> {
    let mut entry = ACameraMetadata_const_entry {
        tag: 0,
        type_: 0,
        count: 0,
        data: ACameraMetadata_const_entry__bindgen_ty_1 { u8_: ptr::null() },
    };
    // SAFETY: `meta` is live, and `entry` is a valid out pointer.
    let status = unsafe { ACameraMetadata_getConstEntry(meta, tag.0, &mut entry) };
    (status == camera_status_t::ACAMERA_OK && entry.count > 0).then_some(entry)
}

/// Whether the camera permission is granted. `None` when the system cannot be
/// asked.
fn permission_granted() -> Option<bool> {
    with_activity(|env, activity| {
        let name = env.new_string(CAMERA)?;
        let state = env
            .call_method(
                activity,
                jni_str!("checkSelfPermission"),
                jni_sig!((java.lang.String) -> jint),
                &[JValue::Object(&name)],
            )?
            .i()?;
        Ok(state == GRANTED)
    })
    .ok()
}

/// Asks the person for the camera permission, as the activity's own request. The
/// answer is not received: it is polled (`wait_for_permission`).
fn request_permission() -> Result<(), String> {
    with_activity(|env, activity| {
        let name = env.new_string(CAMERA)?;
        let names = JObjectArray::<JObject>::new(env, 1, JObject::null())?;
        names.set_element(env, 0, &name)?;
        env.call_method(
            activity,
            jni_str!("requestPermissions"),
            jni_sig!(([java.lang.String], jint) -> void),
            &[JValue::Object(&names), JValue::Int(REQUEST_CODE)],
        )?;
        Ok(())
    })
    .map_err(|error| format!("the camera permission could not be asked: {error}"))
}

/// Whether the window has the focus. A system dialog takes it away. `None` when
/// the window cannot be asked.
fn window_focused() -> Option<bool> {
    with_activity(|env, activity| {
        let window = env
            .call_method(
                activity,
                jni_str!("getWindow"),
                jni_sig!(() -> android.view.Window),
                &[],
            )?
            .l()?;
        let decor = env
            .call_method(
                &window,
                jni_str!("getDecorView"),
                jni_sig!(() -> android.view.View),
                &[],
            )?
            .l()?;
        env.call_method(
            &decor,
            jni_str!("hasWindowFocus"),
            jni_sig!(() -> jboolean),
            &[],
        )?
        .z()
    })
    .ok()
}

/// How far the display is turned clockwise, in degrees: 0, 90, 180 or 270. A
/// display that cannot be asked is taken as upright.
fn display_degrees() -> i32 {
    with_activity(|env, activity| {
        let manager = env
            .call_method(
                activity,
                jni_str!("getWindowManager"),
                jni_sig!(() -> android.view.WindowManager),
                &[],
            )?
            .l()?;
        let display = env
            .call_method(
                &manager,
                jni_str!("getDefaultDisplay"),
                jni_sig!(() -> android.view.Display),
                &[],
            )?
            .l()?;
        env.call_method(&display, jni_str!("getRotation"), jni_sig!(() -> jint), &[])?
            .i()
    })
    .map_or(0, |rotation| rotation.rem_euclid(4) * 90)
}

/// `camera_status_t` as a result: `Ok` for `ACAMERA_OK`, and otherwise the step
/// that failed and the status it gave.
fn camera_ok(status: camera_status_t, what: &str) -> Result<(), String> {
    if status == camera_status_t::ACAMERA_OK {
        Ok(())
    } else {
        Err(format!("{what} failed with camera status {}", status.0))
    }
}

/// `media_status_t` as a result, as `camera_ok` is for the camera.
fn media_ok(status: media_status_t, what: &str) -> Result<(), String> {
    if status == media_status_t::AMEDIA_OK {
        Ok(())
    } else {
        Err(format!("{what} failed with media status {}", status.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On an emulator with no camera (`-camera-back none -camera-front none`), a
    /// scan is refused at once, and its reply is never given. A device with a back
    /// camera has nothing to check here, so the test passes on one. It needs no
    /// JNI: the camera is looked for before the permission is.
    #[test]
    fn a_scan_with_no_back_camera_is_refused_at_once() {
        let manager = Manager::new().expect("the camera service is available");
        if back_camera(&manager).is_ok() {
            return;
        }
        drop(manager);
        let (sent, received) = std::sync::mpsc::channel::<Option<Vec<u8>>>();
        let refused = start(Box::new(move |text| {
            let _ = sent.send(text);
        }));
        assert_eq!(
            refused.expect_err("a scan with no back camera is refused"),
            "this device has no back camera"
        );
        assert!(
            received.try_recv().is_err(),
            "a refused scan gives no reply"
        );
    }
}
