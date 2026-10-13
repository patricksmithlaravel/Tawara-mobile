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
    MainThreadMarker, NSArray, NSCopying, NSDate, NSDictionary, NSFileManager,
    NSFileProtectionComplete, NSFileProtectionKey, NSHomeDirectory, NSNotification,
    NSNotificationCenter, NSNotificationName, NSNumber, NSOperationQueue, NSString, NSURL,
    NSURLIsExcludedFromBackupKey,
};
use objc2_ui_kit::{
    UIActivityTypeCopyToPasteboard, UIActivityViewController, UIApplication,
    UIApplicationDidBecomeActiveNotification, UIApplicationDidEnterBackgroundNotification,
    UIApplicationWillResignActiveNotification, UIBackgroundTaskInvalid, UIColor, UIPasteboard,
    UIPasteboardOptionExpirationDate, UIPasteboardOptionLocalOnly, UIView, UIViewAutoresizing,
    UIWindow,
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
/// UIKit refused. On an iPhone 17 simulator with iOS 27, the first sheet of a
/// run was shown 2.4 s after it was asked for and a second one 0.6 s after
/// (2026-10-13); five seconds leaves room for a slower phone without holding
/// up a press that follows a refusal for long.
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

/// How long a copy stays on the pasteboard (docs/DECISIONS.md D32 item 10):
/// long enough to paste it into another application, short enough that a
/// copied destination or transaction does not linger.
const PASTEBOARD_LIFETIME: Duration = Duration::from_secs(120);

/// The pasteboard. iced's clipboard writes the same pasteboard at the pin, but
/// with no protection, so this write stays Tawara's: a copy is local to this
/// device, so it does not reach the person's other devices, and it expires
/// after `PASTEBOARD_LIFETIME`, so it does not stay on the pasteboard. A
/// destination or a signed spend is not secret, but it should not linger or
/// sync (docs/DECISIONS.md D32 item 9 (d) and item 10).
fn copy(text: &str) {
    // The uniform type identifier of plain UTF-8 text.
    const TEXT: &str = "public.utf8-plain-text";
    let options = pasteboard_options();
    // SAFETY: Foundation and UIKit calls with valid objects, on the main
    // thread, where iced runs the application's update.
    unsafe {
        let kind = NSString::from_str(TEXT);
        let string = Retained::into_super(Retained::into_super(NSString::from_str(text)));
        let item = NSDictionary::from_vec(&[&*kind], vec![string]);
        let items = NSArray::from_vec(vec![item]);
        UIPasteboard::generalPasteboard().setItems_options(&items, &options);
    }
    eprintln!(
        "TAWARA clipboard: local only, expiring after {} s",
        PASTEBOARD_LIFETIME.as_secs()
    );
}

/// The options every copy carries: local to this device, and expiring
/// `PASTEBOARD_LIFETIME` from now (docs/DECISIONS.md D32 item 10). They are
/// built here, apart from the copy, so that the test below can read them: the
/// simulator can observe the expiry but not the local-only option, since it
/// has no Universal Clipboard.
fn pasteboard_options() -> Retained<NSDictionary<NSString, AnyObject>> {
    // SAFETY: Foundation constructors, and UIKit's own option keys, which are
    // immutable constants.
    unsafe {
        let local_only = Retained::into_super(Retained::into_super(Retained::into_super(
            NSNumber::new_bool(true),
        )));
        let expires = Retained::into_super(Retained::into_super(
            NSDate::dateWithTimeIntervalSinceNow(PASTEBOARD_LIFETIME.as_secs_f64()),
        ));
        NSDictionary::from_vec(
            &[
                UIPasteboardOptionLocalOnly,
                UIPasteboardOptionExpirationDate,
            ],
            vec![local_only, expires],
        )
    }
}

/// The share sheet, through `UIActivityViewController` (docs/DECISIONS.md D32
/// item 10 (i)). `name` is not shown: UIKit's sheet takes the text itself, and
/// a subject would need an item provider that the shell does not write (D4).
/// The application cannot learn what was done with the sheet. `Ok` says that
/// UIKit was asked to present it, not that it was shown: the owner decided
/// that on 2026-10-13 (D32 item 10 (i)), and [`present_share`] says why.
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
/// shown. On the simulator (iOS 27, 2026-10-13), straight after the call
/// neither the sheet's presenting controller nor the top controller's
/// presented one was set; the sheet was shown 0.6 to 2.4 s later. A second
/// sheet presented while the first was still waiting was refused by UIKit
/// with only a console message ("Attempt to present ... which is waiting for
/// a delayed presentation"), and its completion was never called.
///
/// So the refusals that can be known are checked first, and that second
/// sheet, which Save artifact pressed twice would ask for, is refused here
/// rather than by UIKit, so that the wallet hears of it. A refusal UIKit makes
/// after this returns is not reported. The owner decided on 2026-10-13 that
/// `Ok` means the platform was asked to show the sheet, not that it was shown
/// (D32 item 10 (i); Tawara-mobile#8). Such a refusal is logged as "never
/// shown" at the next press after `OPENING_LIMIT`. The presentation's
/// completion clears the opening state and logs that the sheet was shown.
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
        // `copy`, so it is left out, and the sheet adds no way to the
        // pasteboard without them. On the simulator the sheet showed no Copy
        // (2026-10-13).
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
/// window it could pick. `UIWindowScene`'s `keyWindow` was considered in its
/// place (D32 item 9), but it would first need the scene found among
/// `connectedScenes`, which adds code for a choice this application does not
/// have.
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
/// The line logged here says whether a cover was added, and the simulator
/// check in platform/ios/tawara.sh reads it, so a missing key window must
/// not pass for a cover (D32 item 9).
fn cover(mtm: MainThreadMarker) {
    COVER.with_borrow_mut(|cover| {
        if cover.is_some() {
            eprintln!("TAWARA lifecycle: inactive; already covered");
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
                eprintln!("TAWARA lifecycle: inactive; no cover: no key window");
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
            eprintln!("TAWARA lifecycle: inactive; cover added");
        }
    });
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

#[cfg(test)]
mod tests {
    use objc2::{ClassType, msg_send};

    use super::*;

    /// The copy, run on the simulator by platform/ios/tawara.sh, which reads
    /// the pasteboard from outside with `simctl pbpaste` while the copy waits
    /// and again once its lifetime has passed (docs/DECISIONS.md D32 item 10).
    /// The local-only option cannot be observed there, so the test first
    /// asserts that the options hold exactly the two keys, local-only set to
    /// YES and an expiry about `PASTEBOARD_LIFETIME` away. The test does not
    /// read the pasteboard itself: from this process a read did not return
    /// the copy, so the observation is made from outside. The test waits out
    /// the lifetime, so it takes over two minutes. `--test-threads=1` keeps
    /// it on the main thread, where the copy's calls belong.
    #[test]
    fn a_copy_for_the_simulator_check() {
        let options = pasteboard_options();
        assert_eq!(options.count(), 2, "only the two options");
        // SAFETY: each value is checked to be of its class before a message
        // of that class is sent to it; the keys are UIKit's own constants.
        unsafe {
            let local_only = options
                .get(UIPasteboardOptionLocalOnly)
                .expect("the local-only option");
            let is_number: bool = msg_send![local_only, isKindOfClass: NSNumber::class()];
            assert!(is_number, "local-only is an NSNumber");
            let on: bool = msg_send![local_only, boolValue];
            assert!(on, "local-only is YES");
            let expires = options
                .get(UIPasteboardOptionExpirationDate)
                .expect("the expiry option");
            let is_date: bool = msg_send![expires, isKindOfClass: NSDate::class()];
            assert!(is_date, "the expiry is an NSDate");
            let left: f64 = msg_send![expires, timeIntervalSinceNow];
            assert!(
                (110.0..=120.0).contains(&left),
                "the expiry is {left} s away"
            );
        }
        copy("tawara-copy-check");
        println!("TAWARA clipboard check: copied");
        std::thread::sleep(PASTEBOARD_LIFETIME + Duration::from_secs(1));
    }
}
