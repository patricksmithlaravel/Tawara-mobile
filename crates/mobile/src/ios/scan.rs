//! iOS: the QR scanner, through AVFoundation, with its bindings written by hand
//! on objc2 0.5 and no crate of their own (docs/DECISIONS.md D42). `ios.rs`
//! gives the application this scanner through the `Host` fields `scan`,
//! `cancel_scan` and `scan_preview`, and the application reads them through
//! `tawara_app::scan`.
//!
//! The scanner is a full-screen modal over the key window, presented the way
//! the share sheet is (`ios.rs`): the camera's preview and a Cancel button. The
//! first reading that holds one distinct text goes back to the application
//! once, through the `done` it was given, and then the camera stops and the
//! scanner goes. The text is not checked here: the application trims it,
//! bounds it and checks it as a destination (docs/DECISIONS.md D42 item 4).
//!
//! Threads: the scanner starts, and is cancelled, on the main thread, where
//! UIKit is. The session starts and stops on a serial queue of its own, since
//! AVFoundation blocks in both calls, and the readings come on another serial
//! queue. A reply may come from either queue, as `ScanDone` allows. The scan
//! under way is kept in `SCAN`: whichever of a reading, the Cancel button, the
//! shell's cancel or a refusal of the camera comes first takes it, so the reply
//! goes back once and the camera stops once.
//!
//! What this writes on stderr is one `TAWARA scan` line per fact, never a text
//! that was read.

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{
    ClassType, DeclaredClass, ProtocolType, class, declare_class, extern_protocol, msg_send,
    msg_send_id, mutability, sel,
};
use objc2_foundation::{
    CGPoint, CGRect, CGSize, MainThreadMarker, NSArray, NSCopying, NSData, NSOperationQueue,
    NSString,
};
use objc2_ui_kit::{
    UIButton, UIButtonType, UIColor, UIControlEvents, UIControlState, UIInterfaceOrientationMask,
    UIModalPresentationStyle, UIView, UIViewAutoresizing, UIViewController,
};
use tawara_app::host::ScanDone;

// AVFoundation, linked so that its classes are loaded when they are first named:
// `class!` panics for a class that is not (keychain.rs does the same for
// LocalAuthentication). The three constants are AVFoundation's own `NSString`s.
#[link(name = "AVFoundation", kind = "framework")]
#[allow(non_upper_case_globals)]
unsafe extern "C" {
    static AVMediaTypeVideo: &'static NSString;
    static AVMetadataObjectTypeQRCode: &'static NSString;
    static AVLayerVideoGravityResizeAspectFill: &'static NSString;
}

// libdispatch, which libSystem holds: a serial queue, and a C function run on one
// with its context. A C function serves where a block would be needed.
unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> *mut AnyObject;
    fn dispatch_async_f(
        queue: *mut AnyObject,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
}

/// `AVAuthorizationStatusNotDetermined` (AVCaptureDevice.h).
const NOT_DETERMINED: isize = 0;
/// `AVAuthorizationStatusAuthorized` (AVCaptureDevice.h). The two refusals are
/// the other statuses.
const AUTHORIZED: isize = 3;

/// `NSUTF8StringEncoding` (NSString.h): the encoding a reading's text is taken as.
const UTF8: usize = 4;

/// The longest text, in UTF-16 units, that a reading is converted from. A QR code
/// holds at most 2953 bytes (version 40, with the lowest error correction), so a
/// longer text is a misreading, and it is not converted.
const LONGEST: usize = 4096;

/// Portrait, for a preview connection that has no rotation angle, which is
/// before iOS 17: `AVCaptureVideoOrientationPortrait` (AVCaptureSession.h).
const PORTRAIT_ORIENTATION: isize = 1;
/// Portrait as an angle, for iOS 17 and later.
const PORTRAIT_ANGLE: f64 = 90.0;

/// The scan under way, from its start until its reply.
struct Scan {
    /// The number of the scan, so that an answer to a request for the camera
    /// finds the scan that made it.
    number: u64,
    /// Where the text goes, once.
    done: ScanDone,
    /// The camera, once it is set up. A scan still asking for access has none.
    running: Option<Running>,
}

/// The camera of a scan that is set up.
struct Running {
    session: Retained<AnyObject>,
    /// The output that reads the codes. Its address tells the readings of this
    /// scan from those of another.
    output: Retained<AnyObject>,
}

impl Running {
    /// Whether `address` is the address of this scan's output.
    fn is_output(&self, address: usize) -> bool {
        Retained::as_ptr(&self.output) as usize == address
    }
}

// SAFETY: the session is sent to the session queue, which is the only place it is
// started or stopped, and the output is only compared by address. Both are
// Objective-C objects, which Rust counts; the scan under way keeps them until
// their reply, and the queue keeps the session until it is stopped.
unsafe impl Send for Running {}

/// The scan under way, if there is one. Its lock is held only to read or change
/// it, and never across a call to UIKit or to a reply.
static SCAN: Mutex<Option<Scan>> = Mutex::new(None);

/// The number of the last scan begun.
static SCANS: AtomicU64 = AtomicU64::new(0);

/// An Objective-C object the scanner keeps for as long as the process runs, so
/// that no thread can message a freed one. Each is thread-safe by its own nature:
/// the two queues, and the target, which has no state.
struct Kept(Retained<AnyObject>);

// SAFETY: see `Kept`. Only messages that the object's own thread rules allow are
// sent to it from another thread.
unsafe impl Send for Kept {}
// SAFETY: see `Kept`.
unsafe impl Sync for Kept {}

/// What every scan shares, made on the first scan and kept.
struct Shared {
    /// The serial queue the session starts and stops on.
    session: Kept,
    /// The serial queue the readings come on.
    readings: Kept,
    /// Receives the readings, and the Cancel button's press.
    target: Kept,
}

static SHARED: OnceLock<Shared> = OnceLock::new();

thread_local! {
    /// The controller the scanner is shown by, while it is up. Main thread only.
    static SHEET: RefCell<Option<Retained<UIViewController>>> = const { RefCell::new(None) };
}

/// What [`Host::scan`](tawara_app::Host) is: starts a scan on the main thread, and
/// shows the scanner. `done` gets the text of the first reading, once, or `None`
/// where the scan ends with no text. `Err` says why the scan did not start, in
/// words for the person (docs/DECISIONS.md D42).
pub(super) fn start(done: ScanDone) -> Result<(), String> {
    // The camera is looked for first, since that reads no state of the interface's:
    // a device with no camera is refused whichever thread asks.
    let Some(device) = video_device() else {
        eprintln!("TAWARA scan: no camera");
        return Err("this device has no camera".to_owned());
    };
    let mtm = MainThreadMarker::new().ok_or("the scanner can only start on the main thread")?;
    if scan_slot().is_some() {
        return Err("a scan is already under way".to_owned());
    }
    if SHEET.with_borrow(Option::is_some) {
        return Err("the scanner is still closing".to_owned());
    }
    shared()?;
    let number = SCANS.fetch_add(1, Ordering::SeqCst) + 1;
    match authorization() {
        AUTHORIZED => {
            *scan_slot() = Some(Scan {
                number,
                done,
                running: None,
            });
            match open(mtm, &device) {
                Ok(opened) => {
                    attach(number, opened);
                    Ok(())
                }
                Err(why) => {
                    forget(number);
                    Err(why)
                }
            }
        }
        NOT_DETERMINED => {
            *scan_slot() = Some(Scan {
                number,
                done,
                running: None,
            });
            ask(number);
            Ok(())
        }
        _ => {
            eprintln!("TAWARA scan: camera access is off");
            Err("camera access is off for Tawara; allow it in Settings".to_owned())
        }
    }
}

/// What `Host::cancel_scan` is: ends the scan under way, if there is one, with no
/// text. Any thread; with no scan under way it does nothing.
pub(super) fn cancel() {
    end_if(|_| true, None, "cancelled");
}

/// Ends the scan under way for a move to the background. The application's
/// own stop follows through `cancel`, but the lock is asked for on the main
/// thread first, so that no camera runs while the application is in the
/// background (docs/DECISIONS.md D42 item 4).
pub(super) fn background() {
    end_if(|_| true, None, "moved to the background");
}

/// Ends the scan under way, when `ends` takes it: its reply goes back once, with
/// `reading`, and then its camera stops and its screen goes. `why` says why, for
/// the log. Returns whether a scan ended. Any thread.
fn end_if(ends: impl FnOnce(&Scan) -> bool, reading: Option<Vec<u8>>, why: &str) -> bool {
    let scan = {
        let mut slot = scan_slot();
        let ends_it = slot.as_ref().is_some_and(ends);
        if ends_it { slot.take() } else { None }
    };
    let Some(Scan { done, running, .. }) = scan else {
        return false;
    };
    eprintln!("TAWARA scan: ended, {why}");
    done(reading);
    if let Some(Running { session, output }) = running {
        enqueue(Order::Stop(session));
        drop(output);
    }
    dismiss();
    true
}

/// The scan under way, locked for a read or a change only.
fn scan_slot() -> MutexGuard<'static, Option<Scan>> {
    SCAN.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Forgets scan `number`, if it is still the one under way, without a reply. Its
/// start was refused, and `start` says so to the application.
fn forget(number: u64) {
    let mut slot = scan_slot();
    if slot.as_ref().is_some_and(|scan| scan.number == number) {
        slot.take();
    }
}

/// The shared objects, made on the first scan, which is on the main thread.
fn shared() -> Result<&'static Shared, String> {
    if let Some(shared) = SHARED.get() {
        return Ok(shared);
    }
    let session = serial_queue(c"tawara.scan.session")?;
    let readings = serial_queue(c"tawara.scan.readings")?;
    // SAFETY: `new` returns a new target of the class, owned, which `Retained`
    // takes over; the cast changes only the static type.
    let target = unsafe {
        let target: Retained<ScanTarget> = msg_send_id![ScanTarget::class(), new];
        Kept(Retained::cast(target))
    };
    let _ = SHARED.set(Shared {
        session,
        readings,
        target,
    });
    SHARED
        .get()
        .ok_or_else(|| "the scanner could not be made".to_owned())
}

/// A new serial queue, which is kept for the life of the process.
fn serial_queue(label: &CStr) -> Result<Kept, String> {
    // SAFETY: a valid C string and no attributes, which make a serial queue. The
    // queue is returned owned, and `Retained` takes that reference.
    let queue = unsafe { dispatch_queue_create(label.as_ptr(), ptr::null()) };
    // SAFETY: `queue` is the owned reference that `dispatch_queue_create` returned.
    unsafe { Retained::from_raw(queue) }
        .map(Kept)
        .ok_or_else(|| "a queue for the scanner could not be made".to_owned())
}

/// What the session queue is asked to do. The queue runs these one after another,
/// so a stop asked after a start is done after it.
enum Order {
    Start(Retained<AnyObject>),
    Stop(Retained<AnyObject>),
}

/// Asks the session queue to run `order`.
fn enqueue(order: Order) {
    let Some(shared) = SHARED.get() else {
        return;
    };
    let context = Box::into_raw(Box::new(order)).cast::<c_void>();
    // SAFETY: the queue is a live serial queue that `shared` keeps, and `run_order`
    // takes the box back once, on that queue.
    unsafe {
        dispatch_async_f(
            Retained::as_ptr(&shared.session.0).cast_mut(),
            context,
            run_order,
        );
    }
}

/// Runs one order on the session queue. The reference the order carries is
/// released once it is done.
extern "C" fn run_order(context: *mut c_void) {
    // SAFETY: `enqueue` made `context` from a box, and this is its only use.
    let order = unsafe { Box::from_raw(context.cast::<Order>()) };
    match *order {
        Order::Start(session) => {
            // SAFETY: the session is one the scan made, started on the queue that
            // starts sessions.
            unsafe {
                let _: () = msg_send![&*session, startRunning];
            }
        }
        Order::Stop(session) => {
            // SAFETY: the session is one the scan made, stopped on the queue that
            // stops it.
            unsafe {
                let _: () = msg_send![&*session, stopRunning];
            }
        }
    }
}

/// Takes the scanner's screen down, from any thread: here on the main thread, and
/// on the main queue otherwise.
fn dismiss() {
    if MainThreadMarker::new().is_some() {
        dismiss_here();
    } else {
        let block = RcBlock::new(dismiss_here);
        // SAFETY: the main queue is always valid, and the block is one the queue
        // copies.
        unsafe { NSOperationQueue::mainQueue().addOperationWithBlock(&block) };
    }
}

/// Takes the scanner's screen down, if it is up. Main thread only.
fn dismiss_here() {
    if let Some(controller) = SHEET.with_borrow_mut(Option::take) {
        // SAFETY: a UIKit call on the main thread, on the controller that was
        // presented there.
        unsafe { controller.dismissViewControllerAnimated_completion(true, None) };
    }
}

/// The default camera, or `None` where the device has none, as the simulator does.
fn video_device() -> Option<Retained<AnyObject>> {
    // SAFETY: a class method of AVCaptureDevice, with the media type's constant.
    // It returns nil where there is no camera.
    unsafe {
        msg_send_id![
            class!(AVCaptureDevice),
            defaultDeviceWithMediaType: AVMediaTypeVideo
        ]
    }
}

/// The camera's access for video, as `AVAuthorizationStatus` gives it.
fn authorization() -> isize {
    // SAFETY: a class method of AVCaptureDevice, with the media type's constant.
    unsafe {
        msg_send![
            class!(AVCaptureDevice),
            authorizationStatusForMediaType: AVMediaTypeVideo
        ]
    }
}

/// Asks for the camera's access, for scan `number`. The answer comes on a thread
/// of the system's, and `answered` takes it up on the main thread.
fn ask(number: u64) {
    // The argument is Objective-C's BOOL, which is `Bool` to objc2: `bool` has no
    // encoding of its own for a block to take.
    let answer = RcBlock::new(move |granted: Bool| {
        let granted = granted.as_bool();
        let task = RcBlock::new(move || answered(number, granted));
        // SAFETY: the main queue is always valid, and the block is one the queue
        // copies.
        unsafe { NSOperationQueue::mainQueue().addOperationWithBlock(&task) };
    });
    // SAFETY: a class method of AVCaptureDevice, with the media type's constant and
    // a completion block that AVFoundation copies.
    unsafe {
        let _: () = msg_send![
            class!(AVCaptureDevice),
            requestAccessForMediaType: AVMediaTypeVideo,
            completionHandler: &*answer
        ];
    }
}

/// The answer to the request for scan `number`, on the main thread. A refusal
/// ends the scan with no text. An access shows the scanner and starts the camera.
fn answered(number: u64, granted: bool) {
    let waiting = scan_slot()
        .as_ref()
        .is_some_and(|scan| scan.number == number && scan.running.is_none());
    if !waiting {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        end_if(|scan| scan.number == number, None, "not on the main thread");
        return;
    };
    if !granted {
        end_if(|scan| scan.number == number, None, "camera access refused");
        return;
    }
    let Some(device) = video_device() else {
        end_if(|scan| scan.number == number, None, "no camera");
        return;
    };
    match open(mtm, &device) {
        Ok(opened) => {
            attach(number, opened);
        }
        Err(why) => {
            eprintln!("TAWARA scan: {why}");
            end_if(|scan| scan.number == number, None, "could not be shown");
        }
    }
}

/// What a scan's camera and screen are, once they are made.
struct Opened {
    session: Retained<AnyObject>,
    output: Retained<AnyObject>,
    controller: Retained<UIViewController>,
}

/// Makes the camera and the scanner's screen, and presents the screen over the top
/// of the key window's controllers, as the share sheet is presented (`ios.rs`).
/// The camera does not start here: `attach` starts it, once the scan is still
/// under way.
fn open(mtm: MainThreadMarker, device: &AnyObject) -> Result<Opened, String> {
    let shared = shared()?;
    let (top, bounds, inset) = presenting(mtm)?;
    let (session, output) = camera(device, shared)?;
    let controller = screen(mtm, bounds, inset, shared, &session)?;
    // SAFETY: UIKit calls on the main thread, with the controller made above.
    let shown = unsafe {
        top.presentViewController_animated_completion(&controller, true, None);
        top.presentedViewController()
    };
    let presented = shown.is_some_and(|presented| {
        ptr::eq(Retained::as_ptr(&presented), Retained::as_ptr(&controller))
    });
    if !presented {
        // SAFETY: a UIKit call on the main thread, in case UIKit queued the
        // presentation after all.
        unsafe { controller.dismissViewControllerAnimated_completion(true, None) };
        return Err("the scanner could not be shown".to_owned());
    }
    Ok(Opened {
        session,
        output,
        controller,
    })
}

/// The top of the key window's controllers, which the scanner is presented from,
/// with the window's bounds and its top safe-area inset, which place the Cancel
/// button. It is refused where the screen cannot take a presentation now, by the
/// same checks the share sheet makes (`ios.rs`, `present_share`).
fn presenting(mtm: MainThreadMarker) -> Result<(Retained<UIViewController>, CGRect, f64), String> {
    let window = super::key_window(mtm).ok_or("no key window")?;
    // SAFETY: UIKit reads on the main thread, of objects UIKit owns.
    unsafe {
        let mut top = window
            .rootViewController()
            .ok_or("no root view controller")?;
        while let Some(presented) = top.presentedViewController() {
            top = presented;
        }
        if top.isBeingPresented() || top.isBeingDismissed() {
            return Err("another screen is still opening or closing".to_owned());
        }
        if !top
            .viewIfLoaded()
            .is_some_and(|view| view.window().is_some())
        {
            return Err("the app's view is not on screen".to_owned());
        }
        Ok((top, window.bounds(), window.safeAreaInsets().top))
    }
}

/// The session that reads `device`'s codes, with a metadata output for QR codes
/// that gives its readings to the readings queue. Each input and output is
/// checked before it is added, since AVFoundation raises an exception for one it
/// cannot take, and an uncaught exception ends the application.
fn camera(
    device: &AnyObject,
    shared: &Shared,
) -> Result<(Retained<AnyObject>, Retained<AnyObject>), String> {
    // SAFETY: AVFoundation's constructors and the session's calls, on the main
    // thread, with objects made here.
    unsafe {
        let session: Retained<AnyObject> = msg_send_id![class!(AVCaptureSession), new];
        let input: Option<Retained<AnyObject>> = msg_send_id![
            class!(AVCaptureDeviceInput),
            deviceInputWithDevice: device,
            error: ptr::null_mut::<*mut AnyObject>()
        ];
        let input = input.ok_or("the camera could not be read")?;
        let can_input: bool = msg_send![&*session, canAddInput: &*input];
        if !can_input {
            return Err("the session takes no camera".to_owned());
        }
        let _: () = msg_send![&*session, addInput: &*input];
        let output: Retained<AnyObject> = msg_send_id![class!(AVCaptureMetadataOutput), new];
        let can_output: bool = msg_send![&*session, canAddOutput: &*output];
        if !can_output {
            return Err("the session takes no code reader".to_owned());
        }
        let _: () = msg_send![&*session, addOutput: &*output];
        // The types are read after the output is added: before that, none are
        // offered. A type that is not offered is refused by the setter, which
        // raises an exception.
        let offered: Retained<AnyObject> = msg_send_id![&*output, availableMetadataObjectTypes];
        let qr: &NSString = AVMetadataObjectTypeQRCode;
        let reads_qr: bool = msg_send![&*offered, containsObject: qr];
        if !reads_qr {
            return Err("this camera reads no QR codes".to_owned());
        }
        let _: () = msg_send![
            &*output,
            setMetadataObjectsDelegate: &*shared.target.0,
            queue: &*shared.readings.0
        ];
        let types = NSArray::from_vec(vec![qr.copy()]);
        let _: () = msg_send![&*output, setMetadataObjectTypes: &*types];
        Ok((session, output))
    }
}

/// The scanner's screen: the camera's preview, which fills it, and a Cancel
/// button at its top right, below the status bar. The screen takes portrait only.
fn screen(
    mtm: MainThreadMarker,
    bounds: CGRect,
    inset: f64,
    shared: &Shared,
    session: &AnyObject,
) -> Result<Retained<UIViewController>, String> {
    // SAFETY: UIKit and AVFoundation calls on the main thread, with objects made
    // here and the target that `shared` keeps.
    unsafe {
        let preview: Retained<PreviewView> =
            msg_send_id![mtm.alloc::<PreviewView>(), initWithFrame: bounds];
        let view: Retained<UIView> = Retained::into_super(preview);
        view.setAutoresizingMask(
            UIViewAutoresizing::FlexibleWidth | UIViewAutoresizing::FlexibleHeight,
        );
        view.setBackgroundColor(Some(&UIColor::blackColor()));
        // The view's layer, which its class makes the preview layer. Read through
        // the runtime: UIKit's binding for it needs the Core Animation crate.
        let layer: Retained<AnyObject> = msg_send_id![&*view, layer];
        let _: () = msg_send![&*layer, setSession: session];
        let gravity: &NSString = AVLayerVideoGravityResizeAspectFill;
        let _: () = msg_send![&*layer, setVideoGravity: gravity];
        let connection: Option<Retained<AnyObject>> = msg_send_id![&*layer, connection];
        if let Some(connection) = connection {
            portrait(&connection);
        }

        let button = UIButton::buttonWithType(UIButtonType::System, mtm);
        let title = NSString::from_str("Cancel");
        button.setTitle_forState(Some(&title), UIControlState::Normal);
        button.setTintColor(Some(&UIColor::whiteColor()));
        button.addTarget_action_forControlEvents(
            Some(&*shared.target.0),
            sel!(cancelScan:),
            UIControlEvents::UIControlEventTouchUpInside,
        );
        button.setAutoresizingMask(
            UIViewAutoresizing::FlexibleLeftMargin | UIViewAutoresizing::FlexibleBottomMargin,
        );
        button.setFrame(CGRect::new(
            CGPoint::new(bounds.size.width - 16.0 - 96.0, inset + 8.0),
            CGSize::new(96.0, 44.0),
        ));
        view.addSubview(&button);

        let controller: Retained<ScanController> =
            msg_send_id![mtm.alloc::<ScanController>(), init];
        let controller: Retained<UIViewController> = Retained::into_super(controller);
        controller.setView(Some(&view));
        controller
            .setModalPresentationStyle(UIModalPresentationStyle::UIModalPresentationFullScreen);
        Ok(controller)
    }
}

/// Turns a preview connection to portrait, by its rotation angle where the
/// connection has one (iOS 17 and later), and by its orientation before that.
fn portrait(connection: &AnyObject) {
    // SAFETY: `respondsToSelector:` is sent to any object, and each setter only
    // where the connection answers it.
    unsafe {
        let angled: bool = msg_send![connection, respondsToSelector: sel!(setVideoRotationAngle:)];
        if angled {
            let supported: bool =
                msg_send![connection, isVideoRotationAngleSupported: PORTRAIT_ANGLE];
            if supported {
                let _: () = msg_send![connection, setVideoRotationAngle: PORTRAIT_ANGLE];
            }
        } else {
            let _: () = msg_send![connection, setVideoOrientation: PORTRAIT_ORIENTATION];
        }
    }
}

/// Starts the camera of scan `number`, and takes the scanner's screen up, if the
/// scan is still the one under way. A scan that has ended meanwhile gets neither,
/// and its screen is taken down again. Returns whether the scan was attached.
fn attach(number: u64, opened: Opened) -> bool {
    let Opened {
        session,
        output,
        controller,
    } = opened;
    let attached = {
        let mut slot = scan_slot();
        match slot.as_mut() {
            Some(scan) if scan.number == number && scan.running.is_none() => {
                scan.running = Some(Running {
                    session: session.clone(),
                    output,
                });
                true
            }
            _ => false,
        }
    };
    if !attached {
        // SAFETY: a UIKit call on the main thread, on the controller presented above.
        unsafe { controller.dismissViewControllerAnimated_completion(true, None) };
        return false;
    }
    SHEET.with_borrow_mut(|sheet| *sheet = Some(controller));
    enqueue(Order::Start(session));
    eprintln!("TAWARA scan: started");
    true
}

/// A reading of the camera's codes, on the readings queue. Its text goes back as
/// the scan's reply when the reading holds one distinct text, and only from the
/// scan whose output gave the reading (docs/DECISIONS.md D42 item 4).
fn reading(output: &AnyObject, codes: &NSArray<AnyObject>) {
    let texts: Vec<Vec<u8>> = codes.iter().filter_map(code_text).collect();
    let Some(text) = one_text(texts) else {
        return;
    };
    let address = output as *const AnyObject as usize;
    end_if(
        |scan| {
            scan.running
                .as_ref()
                .is_some_and(|running| running.is_output(address))
        },
        Some(text),
        "read",
    );
}

/// The one text a reading holds, when it holds exactly one distinct text. A reading
/// of two different codes at once is not known to be the code that is meant, so it
/// is not taken, and the camera reads on (docs/DECISIONS.md D42 item 4).
fn one_text(mut texts: Vec<Vec<u8>>) -> Option<Vec<u8>> {
    texts.sort();
    texts.dedup();
    if texts.len() == 1 { texts.pop() } else { None }
}

/// The text of one code, as the bytes it is, when the code is a QR code that holds
/// text (AVMetadataMachineReadableCodeObject's `stringValue`).
fn code_text(code: &AnyObject) -> Option<Vec<u8>> {
    // SAFETY: `isKindOfClass:` is sent to an object of any class, with a class that
    // AVFoundation defines.
    let is_code: bool =
        unsafe { msg_send![code, isKindOfClass: class!(AVMetadataMachineReadableCodeObject)] };
    if !is_code {
        return None;
    }
    // SAFETY: the object is a machine-readable code, which has the `stringValue`
    // getter, and the result is an owned or null string.
    let text: Option<Retained<NSString>> = unsafe { msg_send_id![code, stringValue] };
    let text = text?;
    if text.len_utf16() > LONGEST {
        return None;
    }
    // SAFETY: `dataUsingEncoding:` is sent to a string with a valid encoding. The
    // result is owned or null.
    let data: Option<Retained<NSData>> = unsafe { msg_send_id![&*text, dataUsingEncoding: UTF8] };
    Some(data?.bytes().to_vec())
}

// The view the preview is drawn on. Its layer is AVFoundation's preview layer, so
// the camera's frames fill it with no sublayer of its own.
declare_class!(
    struct PreviewView;

    unsafe impl ClassType for PreviewView {
        type Super = UIView;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "TawaraScanPreview";
    }

    impl DeclaredClass for PreviewView {}

    unsafe impl PreviewView {
        #[method(layerClass)]
        fn layer_class() -> &'static AnyClass {
            class!(AVCaptureVideoPreviewLayer)
        }
    }
);

// The controller that shows the scanner. It takes portrait only, so the preview
// stays upright as the device turns, and `portrait` sets the connection to match.
declare_class!(
    struct ScanController;

    unsafe impl ClassType for ScanController {
        type Super = UIViewController;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "TawaraScanController";
    }

    impl DeclaredClass for ScanController {}

    unsafe impl ScanController {
        #[method(supportedInterfaceOrientations)]
        fn supported_interface_orientations(&self) -> UIInterfaceOrientationMask {
            UIInterfaceOrientationMask::Portrait
        }
    }
);

extern_protocol!(
    /// AVFoundation's `AVCaptureMetadataOutputObjectsDelegate` (AVCaptureMetadataOutput.h),
    /// to which the camera's readings are given.
    ///
    /// # Safety
    ///
    /// An implementation is an Objective-C object that AVFoundation can message.
    /// The readings are sent on the queue the output was given, so an
    /// implementation must not block on the scanner's session queue.
    #[allow(clippy::missing_safety_doc)]
    unsafe trait AVCaptureMetadataOutputObjectsDelegate: NSObjectProtocol {
        #[optional]
        #[method(captureOutput:didOutputMetadataObjects:fromConnection:)]
        unsafe fn capture_output_did_output_metadata_objects_from_connection(
            &self,
            output: &AnyObject,
            metadata_objects: &NSArray<AnyObject>,
            connection: &AnyObject,
        );
    }

    unsafe impl ProtocolType for dyn AVCaptureMetadataOutputObjectsDelegate {}
);

// The object the readings and the Cancel button are sent to. It has no state of
// its own, and it is kept for the life of the process (`Kept`).
declare_class!(
    struct ScanTarget;

    unsafe impl ClassType for ScanTarget {
        type Super = NSObject;
        type Mutability = mutability::Immutable;
        const NAME: &'static str = "TawaraScanTarget";
    }

    impl DeclaredClass for ScanTarget {}

    unsafe impl NSObjectProtocol for ScanTarget {}

    unsafe impl AVCaptureMetadataOutputObjectsDelegate for ScanTarget {
        #[method(captureOutput:didOutputMetadataObjects:fromConnection:)]
        unsafe fn capture_output_did_output_metadata_objects_from_connection(
            &self,
            output: &AnyObject,
            metadata_objects: &NSArray<AnyObject>,
            _connection: &AnyObject,
        ) {
            reading(output, metadata_objects);
        }
    }

    unsafe impl ScanTarget {
        #[method(cancelScan:)]
        fn cancel_pressed(&self, _sender: &AnyObject) {
            end_if(|_| true, None, "cancelled");
        }
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_distinct_text_is_taken_and_no_other() {
        assert_eq!(one_text(Vec::new()), None);
        assert_eq!(one_text(vec![b"0xA".to_vec()]), Some(b"0xA".to_vec()));
        assert_eq!(
            one_text(vec![b"0xA".to_vec(), b"0xA".to_vec()]),
            Some(b"0xA".to_vec())
        );
        assert_eq!(one_text(vec![b"0xA".to_vec(), b"0xB".to_vec()]), None);
    }

    /// On the simulator there is no camera, so a scan is refused at once, and its
    /// reply is never given. The test runs on a thread of the test harness, not on
    /// the main thread: the refusal for a missing camera comes before the check for
    /// the main thread, so the test does not need it. A device with a camera has
    /// nothing to check here, so the test passes.
    #[test]
    fn a_scan_with_no_camera_is_refused_at_once() {
        if video_device().is_some() {
            return;
        }
        let (sent, received) = std::sync::mpsc::channel::<Option<Vec<u8>>>();
        let refused = start(Box::new(move |text| {
            let _ = sent.send(text);
        }));
        assert_eq!(
            refused.expect_err("a scan with no camera is refused"),
            "this device has no camera"
        );
        assert!(
            received.try_recv().is_err(),
            "a refused scan gives no reply"
        );
    }
}
