//! iOS: the store's directory, its protection and backup exclusion, the
//! pasteboard, the share sheet, the app-switcher cover and the lock, through
//! the Objective-C runtime with no Swift or Objective-C source
//! (docs/DECISIONS.md D4, D32).
//!
//! Everything here runs on the main thread: [`main`] before winit's
//! `UIApplicationMain`, and the notification blocks on the main queue. What
//! this writes on stderr is one `TAWARA` line per fact, never a secret.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{
    MainThreadMarker, NSArray, NSCopying, NSDictionary, NSFileManager, NSFileProtectionComplete,
    NSFileProtectionKey, NSHomeDirectory, NSNotification, NSNotificationCenter, NSNotificationName,
    NSNumber, NSOperationQueue, NSString, NSURL, NSURLIsExcludedFromBackupKey,
};
use objc2_ui_kit::{
    UIActivityTypeCopyToPasteboard, UIActivityViewController, UIApplication,
    UIApplicationDidBecomeActiveNotification, UIApplicationDidEnterBackgroundNotification,
    UIApplicationWillResignActiveNotification, UIBackgroundTaskInvalid, UIColor, UIPasteboard,
    UIView, UIViewAutoresizing, UIWindow,
};
use tawara_app::Host;

thread_local! {
    /// The cover over the window while the application is not active.
    static COVER: RefCell<Option<Retained<UIView>>> = const { RefCell::new(None) };
    /// The share sheet UIKit has been asked for and has not shown yet: its
    /// number and when it was asked for.
    static OPENING: Cell<Option<(u64, Instant)>> = const { Cell::new(None) };
    /// The number of the last share sheet asked for.
    static SHEETS: Cell<u64> = const { Cell::new(0) };
}

/// How long a share sheet may take to open before it is taken as one that
/// UIKit refused. On the simulator it opens within a second.
const OPENING_LIMIT: Duration = Duration::from_secs(5);

/// Start the application. Never returns: winit's `UIApplicationMain` ends
/// the process.
pub fn main() {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("TAWARA stop: not started on the main thread");
        std::process::exit(1);
    };
    let Some(private_dir) = private_dir() else {
        eprintln!("TAWARA stop: the app-private directory could not be made");
        std::process::exit(1);
    };
    eprintln!("TAWARA start: private directory {}", private_dir.display());
    observe(mtm);
    let host = Host {
        private_dir,
        copy,
        share,
        back: || {},
    };
    if let Err(error) = tawara_app::run_in(host) {
        eprintln!("TAWARA stop: {error}");
        std::process::exit(1);
    }
}

/// `Library/Application Support/Tawara` in the app's container, made mode
/// `0700`, protected with `NSFileProtectionComplete` and excluded from
/// iCloud and device backups (D32 items 3 and 7). The store goes in a
/// folder of its own inside it; the protection is set again on everything
/// already there at each start, so a file made since inherits nothing it
/// should not keep.
fn private_dir() -> Option<PathBuf> {
    // SAFETY: NSHomeDirectory has no preconditions and returns a valid,
    // autoreleased NSString.
    let home = PathBuf::from(unsafe { NSHomeDirectory().as_ref() }.to_string());
    let dir = home.join("Library/Application Support/Tawara");
    {
        use std::os::unix::fs::DirBuilderExt;
        if !dir.is_dir() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir)
                .ok()?;
        }
    }
    let protected = protect(&dir);
    let excluded = exclude_from_backup(&dir);
    eprintln!("TAWARA store: protected={protected} excluded_from_backup={excluded}");
    (protected && excluded).then_some(dir)
}

/// `NSFileProtectionComplete` on `path` and on everything below it.
fn protect(path: &Path) -> bool {
    let mut ok = set_protection(path);
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let child = entry.path();
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            ok &= if is_dir {
                protect(&child)
            } else {
                set_protection(&child)
            };
        }
    }
    ok
}

fn set_protection(path: &Path) -> bool {
    // SAFETY: the keys and values are Foundation's own constants, and the
    // file manager is used as documented, with a valid path.
    unsafe {
        let value: Retained<AnyObject> =
            Retained::into_super(Retained::into_super(NSFileProtectionComplete.copy()));
        let attributes = NSDictionary::from_vec(&[NSFileProtectionKey], vec![value]);
        NSFileManager::defaultManager()
            .setAttributes_ofItemAtPath_error(&attributes, &ns_path(path))
            .map_err(|e| eprintln!("TAWARA protect {}: {e:?}", path.display()))
            .is_ok()
    }
}

/// `NSURLIsExcludedFromBackupKey` on the directory: neither iCloud nor a
/// device backup holds it (docs/PLAN.md section 4.5).
fn exclude_from_backup(dir: &Path) -> bool {
    // SAFETY: Foundation calls with valid objects and Foundation's own key.
    unsafe {
        let url = NSURL::fileURLWithPath_isDirectory(&ns_path(dir), true);
        let yes = NSNumber::new_bool(true);
        url.setResourceValue_forKey_error(Some(&yes), NSURLIsExcludedFromBackupKey)
            .map_err(|e| eprintln!("TAWARA exclude {}: {e:?}", dir.display()))
            .is_ok()
    }
}

fn ns_path(path: &Path) -> Retained<NSString> {
    NSString::from_str(&path.to_string_lossy())
}

/// The pasteboard. iced's clipboard writes the same pasteboard at the pin, but
/// with no protection, so this write stays Tawara's: the expiring, local-only
/// item that D32 item 9 (d) asks for belongs here.
fn copy(text: &str) {
    // SAFETY: plain UIKit calls with valid objects, on the main thread,
    // where iced runs the application's update.
    unsafe { UIPasteboard::generalPasteboard().setString(Some(&NSString::from_str(text))) };
}

/// The share sheet, through `UIActivityViewController` (docs/DECISIONS.md D32
/// item 10 (i)). `name` is not shown: UIKit's sheet takes the text itself, and
/// a subject would need an item provider that the shell does not write (D4).
/// The application cannot learn what was done with the sheet.
fn share(_name: &str, text: &str) -> Result<(), String> {
    let result = present_share(text);
    match &result {
        Ok(()) => eprintln!("TAWARA share: sheet asked for"),
        Err(error) => eprintln!("TAWARA share: {error}"),
    }
    result
}

/// Presents the sheet from the top of the key window's controllers, the same
/// window `cover` covers. The app is iPhone only (UIDeviceFamily 1), so there
/// is no popover to anchor the sheet to.
///
/// `UIActivityViewController` puts off its own presentation until its content
/// is ready, so nothing UIKit offers says at once whether the sheet will be
/// shown: its presenting controller is set some hundreds of milliseconds
/// later, and a refused presentation is only a console warning. So the
/// refusals that can be known are checked first, and the one met in practice,
/// a second sheet asked for while the first is still opening (Save artifact
/// pressed twice), is refused here rather than by UIKit, so that the wallet
/// does not say a sheet was opened when it was not (D32 item 10 (i)). The
/// presentation's completion clears that state and logs that the sheet was
/// shown.
fn present_share(text: &str) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("not on the main thread")?;
    if let Some((_, asked)) = OPENING.get() {
        if asked.elapsed() < OPENING_LIMIT {
            return Err("the share sheet is still opening".to_owned());
        }
        eprintln!("TAWARA share: the last sheet was never shown");
        OPENING.set(None);
    }
    // SAFETY: UIKit calls on the main thread with valid objects. The items are
    // made here; the window and its controllers are UIKit's own, read on the
    // main thread, and the sheet is presented on the top one, as a modal sheet
    // is. The completion block is called on the main thread, where `OPENING`
    // lives.
    unsafe {
        let item: Retained<AnyObject> =
            Retained::into_super(Retained::into_super(NSString::from_str(text)));
        let items = NSArray::from_vec(vec![item]);
        let sheet = UIActivityViewController::initWithActivityItems_applicationActivities(
            mtm.alloc(),
            &items,
            None,
        );
        // The sheet's own Copy would put the signed bytes on the general
        // pasteboard with neither the local-only flag nor the expiry that D32
        // items 9 (d) and 10 (j) ask of every copy, and which belong in
        // `copy`, so it is left out: the wallet's own Copy is the one way to
        // the pasteboard.
        let excluded = NSArray::from_vec(vec![UIActivityTypeCopyToPasteboard.copy()]);
        sheet.setExcludedActivityTypes(Some(&excluded));
        let window = key_window(mtm).ok_or("no key window")?;
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
        let number = SHEETS.get() + 1;
        SHEETS.set(number);
        OPENING.set(Some((number, Instant::now())));
        let shown = RcBlock::new(move || {
            if OPENING.get().is_some_and(|(opening, _)| opening == number) {
                OPENING.set(None);
            }
            eprintln!("TAWARA share: sheet shown");
        });
        top.presentViewController_animated_completion(&sheet, true, Some(&shown));
    }
    Ok(())
}

/// The key window. `keyWindow` is deprecated because it picks one key window
/// across all of an application's connected scenes; Tawara has one scene
/// (`UIApplicationSupportsMultipleScenes` is false in platform/ios's
/// Info.plist.in) and is iPhone only (UIDeviceFamily 1), so there is only one
/// window it could pick.
fn key_window(mtm: MainThreadMarker) -> Option<Retained<UIWindow>> {
    // SAFETY: a UIKit call on the main thread.
    #[allow(deprecated)]
    unsafe {
        UIApplication::sharedApplication(mtm).keyWindow()
    }
}

/// `UIApplication`'s notices, on the main queue. winit hears them too, and two
/// of them reach iced as `Suspended` and `Resumed` events, which iced passes
/// on through `on_lifecycle` and `lifecycle()`. Tawara uses neither on iOS (D32
/// item 9 (d)), so these observers act on the notices. The observers live as
/// long as the process.
fn observe(mtm: MainThreadMarker) {
    type Notice = (&'static NSNotificationName, fn(MainThreadMarker));
    // SAFETY: the notification names are UIKit's own constants.
    let notices: [Notice; 3] = unsafe {
        [
            (UIApplicationWillResignActiveNotification, cover),
            (
                UIApplicationDidEnterBackgroundNotification,
                entered_background,
            ),
            (UIApplicationDidBecomeActiveNotification, uncover),
        ]
    };
    // SAFETY: the main queue and the default center are always valid.
    let (center, queue) = unsafe {
        (
            NSNotificationCenter::defaultCenter(),
            NSOperationQueue::mainQueue(),
        )
    };
    for (name, act) in notices {
        let block = RcBlock::new(move |_: NonNull<NSNotification>| act(mtm));
        // SAFETY: the block is called on the main queue, as `act` needs.
        let observer = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(name),
                None,
                Some(&queue),
                &block,
            )
        };
        std::mem::forget(observer);
    }
}

/// The application is about to stop being active (the app switcher, a
/// call, Control Center): cover the window, so the snapshot iOS takes for
/// the app switcher shows nothing of the wallet (docs/PLAN.md section 4.3).
fn cover(mtm: MainThreadMarker) {
    COVER.with_borrow_mut(|cover| {
        if cover.is_some() {
            return;
        }
        // SAFETY: UIKit calls on the main thread with valid objects.
        unsafe {
            let window = key_window(mtm);
            // The window is shown only in a window scene, which the iced
            // fork puts it in (D32 item 8); read back from UIKit for
            // platform/ios/tawara.sh's check.
            let in_scene = window.as_ref().is_some_and(|w| w.windowScene().is_some());
            eprintln!("TAWARA scene: key window in scene={in_scene}");
            let Some(window) = window else {
                return;
            };
            let view = UIView::initWithFrame(mtm.alloc(), window.bounds());
            let [r, g, b] = background();
            let colour = UIColor::colorWithRed_green_blue_alpha(r, g, b, 1.0);
            view.setBackgroundColor(Some(&colour));
            view.setAutoresizingMask(
                UIViewAutoresizing::FlexibleWidth | UIViewAutoresizing::FlexibleHeight,
            );
            window.addSubview(&view);
            *cover = Some(view);
        }
    });
    eprintln!("TAWARA lifecycle: inactive; covered");
}

/// Active again: the cover goes. The wallet is locked if the application
/// was in the background meanwhile.
fn uncover(_: MainThreadMarker) {
    if let Some(view) = COVER.with_borrow_mut(Option::take) {
        // SAFETY: UIKit call on the main thread with a valid view.
        unsafe { view.removeFromSuperview() };
    }
    eprintln!("TAWARA lifecycle: active; uncovered");
}

/// In the background: the wallet locks there and then, and the process is
/// kept running until the lock is done (D32 item 6). The worker locks after
/// the command it is running, which a node request or a spend being
/// submitted can hold, and iOS may suspend the process as soon as this
/// returns; and what is typed, or a recovery phrase shown or still on its
/// way from the worker, is the interface's to drop, on the main thread. So
/// UIKit is asked for background time, which ends when the interface has
/// taken the worker's answer to the move (`Leaving`). If the time
/// runs out first, the process ends itself rather than be suspended with
/// the store's key in memory (the owner's choice, D32 item 6): the next
/// start is locked, and nothing persistent is lost.
fn entered_background(mtm: MainThreadMarker) {
    eprintln!("TAWARA lifecycle: background; locking");
    let task = Arc::new(Task::default());
    let expired = {
        let task = Arc::clone(&task);
        RcBlock::new(move || {
            if !task.locked.load(Ordering::SeqCst) {
                eprintln!("TAWARA lifecycle: background time ran out before the lock; ending");
                std::process::exit(0);
            }
            // SAFETY: UIKit calls the expiration handler on the main thread.
            task.end(unsafe { MainThreadMarker::new_unchecked() });
        })
    };
    // SAFETY: a UIKit call on the main thread; the name is a valid string
    // and the handler a block UIKit copies.
    let id = unsafe {
        UIApplication::sharedApplication(mtm).beginBackgroundTaskWithName_expirationHandler(
            Some(&NSString::from_str("Tawara: locking the wallet")),
            Some(&expired),
        )
    };
    task.id.store(id, Ordering::SeqCst);
    let leaving = tawara_app::left_foreground();

    // SAFETY: UIBackgroundTaskInvalid is UIKit's own constant.
    if id == unsafe { UIBackgroundTaskInvalid } {
        // No background time at all: the only thread left to wait on is
        // this one, the main thread, which the interface's own wipe needs
        // (D32 item 6), so the wait could not end well. The same rule as
        // when the time runs out.
        drop(leaving);
        eprintln!("TAWARA lifecycle: no background time to finish the lock; ending");
        std::process::exit(0);
    }

    let spawned = std::thread::Builder::new()
        .name("tawara-background".into())
        .spawn(move || {
            while !leaving.wait(Duration::from_secs(1)) {
                if task.ended.load(Ordering::SeqCst) {
                    return;
                }
            }
            task.locked.store(true, Ordering::SeqCst);
            eprintln!("TAWARA lifecycle: background; locked");
            // UIApplication is the main thread's: end the task there.
            let block = RcBlock::new(move || {
                // SAFETY: the main queue runs its operations on the main
                // thread.
                task.end(unsafe { MainThreadMarker::new_unchecked() });
            });
            // SAFETY: the main queue is always valid, and the block is one
            // the queue copies.
            unsafe { NSOperationQueue::mainQueue().addOperationWithBlock(&block) };
        });
    if spawned.is_err() {
        // No thread to wait for the lock on: the same rule as when the
        // background time runs out.
        eprintln!("TAWARA lifecycle: no thread to wait for the lock; ending");
        std::process::exit(0);
    }
}

/// One stretch of background time, shared by its expiration handler and the
/// thread waiting for the lock.
#[derive(Default)]
struct Task {
    id: AtomicUsize,
    locked: AtomicBool,
    ended: AtomicBool,
}

impl Task {
    /// End the background time, once, whichever of the lock and the
    /// expiration comes first.
    fn end(&self, mtm: MainThreadMarker) {
        if !self.ended.swap(true, Ordering::SeqCst) {
            // SAFETY: a UIKit call on the main thread with the identifier
            // UIKit gave.
            unsafe {
                UIApplication::sharedApplication(mtm)
                    .endBackgroundTask(self.id.load(Ordering::SeqCst));
            }
        }
    }
}

/// The application's background colour, as `UIColor` takes it.
fn background() -> [f64; 3] {
    let c = tawara_app::theme::color::BG_APP;
    [f64::from(c.r), f64::from(c.g), f64::from(c.b)]
}
