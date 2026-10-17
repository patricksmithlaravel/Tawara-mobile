//! iOS: the device's own check for the store's password, and the password kept
//! in the Keychain behind that check (docs/DECISIONS.md D45). This module fills
//! the `Host` fields `device_check`, `keep_password`, `release_password` and
//! `forget_password` that `ios.rs` gives the application, through the
//! Objective-C runtime and the Security and LocalAuthentication frameworks, with
//! no Swift or Objective-C source (docs/DECISIONS.md D4).
//!
//! An item is a generic password under one service, labelled with the store's
//! identifier, which is its 32 bytes in lower-case hex. Its access control asks
//! for the device's own check, which accepts Face ID, Touch ID or the passcode,
//! and it is readable only while the device has a passcode, on this device only
//! (docs/DECISIONS.md D45 item 2). Keeping and forgetting run on the caller's
//! thread, as their signatures in `Host` have them. The Keychain's documentation
//! does not say that either one asks the check; the device checks of D45 item 5
//! confirm it. A release waits for the check, so it runs on a thread of its own,
//! and its answer goes back once through the callback it is given.
//!
//! What this writes on stderr is one `TAWARA unlock` line per fact, never a
//! password or a copy of one.

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering, compiler_fence};
use std::sync::{Arc, Mutex, PoisonError};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{ClassType, Message, class, msg_send, msg_send_id};
use objc2_foundation::{
    NSArray, NSCopying, NSData, NSDictionary, NSError, NSMutableData, NSNumber, NSString,
};
use tawara_app::host::{Deliver, DeviceCheck, Released, SecretText};

/// The service every item is filed under. It names the unlock, so that no
/// other item of the application is ever read or deleted as one of these.
const SERVICE: &str = "com.patricksmithlaravel.tawara.unlock";

/// `kSecAccessControlUserPresence` (SecAccessControl.h): the device's own check.
/// `SecAccessControlCreateFlags` is a `CFOptionFlags`, `unsigned long` on 64-bit
/// iOS.
const USER_PRESENCE: u64 = 1 << 0;

/// A Security status (`OSStatus`, SecBase.h).
type OSStatus = i32;

/// `errSecSuccess`, and the codes this module answers for (SecBase.h).
const SUCCESS: OSStatus = 0;
/// The person cancelled the check (`errSecUserCanceled`).
const USER_CANCELED: OSStatus = -128;
/// The check did not pass (`errSecAuthFailed`).
const AUTH_FAILED: OSStatus = -25293;
/// No item matches the query (`errSecItemNotFound`).
const ITEM_NOT_FOUND: OSStatus = -25300;

/// `LAPolicyDeviceOwnerAuthentication`, the device's passcode, and the same
/// policy with biometry (LAPublicDefines.h).
const POLICY_PASSCODE: isize = 2;
const POLICY_BIOMETRY: isize = 1;

/// `LABiometryTypeTouchID` and `LABiometryTypeFaceID` (LABiometryType.h).
const BIOMETRY_TOUCH_ID: isize = 1;
const BIOMETRY_FACE_ID: isize = 2;

/// A dictionary of Keychain attributes, keyed by Security's own constants.
type Dictionary = NSDictionary<NSString, AnyObject>;

/// The release in progress, if any: one check at a time, so that a second press
/// does not stack a second sheet.
static ASKING: AtomicBool = AtomicBool::new(false);

// LocalAuthentication, linked so that `LAContext` is a class at run time. The
// block declares nothing, since `class!` panics for a class that is not loaded.
#[link(name = "LocalAuthentication", kind = "framework")]
unsafe extern "C" {}

#[link(name = "Security", kind = "framework")]
#[allow(non_snake_case, non_upper_case_globals)]
unsafe extern "C" {
    static kSecClass: &'static NSString;
    static kSecClassGenericPassword: &'static NSString;
    static kSecAttrService: &'static NSString;
    static kSecAttrAccount: &'static NSString;
    static kSecAttrAccessControl: &'static NSString;
    static kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly: &'static NSString;
    static kSecValueData: &'static NSString;
    static kSecReturnData: &'static NSString;
    static kSecReturnAttributes: &'static NSString;
    static kSecMatchLimit: &'static NSString;
    static kSecMatchLimitOne: &'static NSString;
    static kSecMatchLimitAll: &'static NSString;
    static kSecUseAuthenticationContext: &'static NSString;

    fn SecItemAdd(attributes: *const c_void, result: *mut *const c_void) -> OSStatus;
    fn SecItemCopyMatching(query: *const c_void, result: *mut *const c_void) -> OSStatus;
    fn SecItemDelete(query: *const c_void) -> OSStatus;
    fn SecAccessControlCreateWithFlags(
        allocator: *const c_void,
        protection: *const c_void,
        flags: u64,
        error: *mut *const c_void,
    ) -> *const c_void;
    fn SecCopyErrorMessageString(status: OSStatus, reserved: *mut c_void) -> *const c_void;
}

/// Whether the device offers a check, and what it is called, read now: `None`
/// where the device has no passcode, so that no check exists (D45 items 1 and
/// 2). Read at each foreground, and not kept.
pub(super) fn device_check() -> Option<DeviceCheck> {
    // SAFETY: `LAContext` is a class of the framework linked above, and `new`
    // returns a new object that `Retained` owns. Each call is the framework's
    // own, on a context made here. Neither `canEvaluatePolicy:error:` shows a
    // sheet, and the error out-pointer is allowed to be null.
    unsafe {
        let context: Retained<AnyObject> = msg_send_id![class!(LAContext), new];
        let passcode: bool = msg_send![
            &*context,
            canEvaluatePolicy: POLICY_PASSCODE,
            error: ptr::null_mut::<AnyObject>()
        ];
        if !passcode {
            return None;
        }
        let biometry: bool = msg_send![
            &*context,
            canEvaluatePolicy: POLICY_BIOMETRY,
            error: ptr::null_mut::<AnyObject>()
        ];
        // Read after the policy is asked, on the same context. It names the
        // check only where biometry is available; otherwise the check is the
        // passcode, which the system accepts on every device that has one.
        let kind: isize = msg_send![&*context, biometryType];
        Some(match (biometry, kind) {
            (true, BIOMETRY_FACE_ID) => DeviceCheck::FaceId,
            (true, BIOMETRY_TOUCH_ID) => DeviceCheck::TouchId,
            _ => DeviceCheck::Passcode,
        })
    }
}

/// Keeps `password` under `identifier`, replacing whatever is kept (D45 item 4).
pub(super) fn keep(identifier: &[u8; 32], password: &str) -> Result<(), String> {
    forget()?;
    let control = access_control()?;
    let account = account(identifier);
    // Mutable, so that the copy the Keychain is given can be zeroed once the
    // Keychain has taken its own.
    let mut copy = NSMutableData::with_bytes(password.as_bytes());
    // The dictionary takes a second reference to the same bytes, so that they
    // can be zeroed once the Keychain has taken its own copy.
    // SAFETY: `copy` keeps the object alive for the call, and the retain takes one
    // more reference, which `Retained` owns.
    let shared = unsafe { Retained::retain(Retained::as_ptr(&copy).cast_mut()) }
        .ok_or_else(|| "the password could not be copied".to_owned())?;
    // SAFETY: Security's own constants, read as they are.
    let attributes = unsafe {
        dictionary(vec![
            (kSecClass, constant(kSecClassGenericPassword)),
            (kSecAttrService, text(SERVICE)),
            (kSecAttrAccount, text(&account)),
            (kSecAttrAccessControl, control),
            (kSecValueData, any(shared)),
        ])
    };
    // SAFETY: a valid dictionary of attributes, and no result is asked for. The
    // access control carries the protection class, so no `kSecAttrAccessible`
    // is set beside it.
    let status = unsafe { SecItemAdd(as_cf(&attributes), ptr::null_mut()) };
    drop(attributes);
    wipe(copy.bytes_mut());
    if status == SUCCESS {
        eprintln!("TAWARA unlock: password kept");
        Ok(())
    } else {
        eprintln!("TAWARA unlock: password not kept: {status}");
        Err(describe(status))
    }
}

/// Deletes what is kept, whatever its label, and nothing when nothing is kept
/// (D45 item 4).
pub(super) fn forget() -> Result<(), String> {
    // SAFETY: Security's own constants, read as they are.
    let query = unsafe {
        dictionary(vec![
            (kSecClass, constant(kSecClassGenericPassword)),
            (kSecAttrService, text(SERVICE)),
        ])
    };
    // SAFETY: a valid query dictionary.
    let status = unsafe { SecItemDelete(as_cf(&query)) };
    match status {
        SUCCESS | ITEM_NOT_FOUND => Ok(()),
        other => {
            eprintln!("TAWARA unlock: not forgotten: {other}");
            Err(describe(other))
        }
    }
}

/// Asks the device's check and reads the kept password back (D45 item 4). The
/// call returns at once. `answer` is called once, later, from a thread of this
/// module's own, and never from the caller's.
pub(super) fn release(identifier: &[u8; 32], reason: &str, answer: Deliver) {
    let pending = Pending::new(answer);
    let answering = pending.clone();
    let identifier = *identifier;
    let reason = reason.to_owned();
    let spawned = std::thread::Builder::new()
        .name("tawara-unlock".into())
        .spawn(move || {
            // Answers `Failed` if the thread ends without one, as a panic does.
            let thread = Answering(answering);
            let released = ask(&identifier, &reason);
            thread.0.finish(released);
        });
    if spawned.is_err() {
        // No thread to ask on. The answer still comes back, from here, rather
        // than never.
        eprintln!("TAWARA unlock: no thread to ask the check on");
        pending.finish(Released::Failed(
            "the check could not be started".to_owned(),
        ));
    }
}

/// The release, on the thread `release` made. It blocks while the check is up.
fn ask(identifier: &[u8; 32], reason: &str) -> Released {
    if reason.trim().is_empty() {
        // The system does not take an empty reason.
        return Released::Failed("the check was given no reason".to_owned());
    }
    let Some(_asking) = Asking::start() else {
        eprintln!("TAWARA unlock: a check is already open");
        return Released::Failed("a check is already open".to_owned());
    };
    let account = account(identifier);
    purge_others(&account);
    let released = read(&account, reason);
    eprintln!("TAWARA unlock: the check answered {}", kind(&released));
    released
}

/// Reads the password under `account`, which asks the check for it.
fn read(account: &str, reason: &str) -> Released {
    // A context of its own. A context that has already passed a check would
    // skip the next one (docs/DECISIONS.md D45 item 2).
    // SAFETY: `LAContext` is a class of the linked framework, and `new` returns
    // a new object that `Retained` owns.
    let context: Retained<AnyObject> = unsafe { msg_send_id![class!(LAContext), new] };
    let reason_text = NSString::from_str(reason);
    // SAFETY: `setLocalizedReason:` takes an NSString, which `reason_text` is.
    let _: () = unsafe { msg_send![&*context, setLocalizedReason: &*reason_text] };
    // SAFETY: Security's own constants, read as they are.
    let query = unsafe {
        dictionary(vec![
            (kSecClass, constant(kSecClassGenericPassword)),
            (kSecAttrService, text(SERVICE)),
            (kSecAttrAccount, text(account)),
            (kSecReturnData, yes()),
            (kSecMatchLimit, constant(kSecMatchLimitOne)),
            (kSecUseAuthenticationContext, any(context)),
        ])
    };
    let mut found: *const c_void = ptr::null();
    // SAFETY: a valid query, and `found` is a valid out pointer. On success the
    // result is a +1 CFData, which `Retained` takes over below.
    let status = unsafe { SecItemCopyMatching(as_cf(&query), &mut found) };
    drop(query);
    match status {
        SUCCESS => {
            // SAFETY: the result is the owned CFData that success returns.
            match unsafe { Retained::<NSData>::from_raw(found.cast_mut().cast()) } {
                Some(data) => secret_of(data),
                None => Released::Failed("the Keychain gave back no password".to_owned()),
            }
        }
        USER_CANCELED | AUTH_FAILED => Released::Declined,
        ITEM_NOT_FOUND => Released::Missing,
        other => Released::Failed(describe(other)),
    }
}

/// The kept password, as text in a secret. The bytes are copied, and the copy is
/// the one the secret zeroes on drop; `data` is the Keychain's own and cannot be
/// zeroed, so it is released here as soon as it is copied.
fn secret_of(data: Retained<NSData>) -> Released {
    let bytes = data.bytes().to_vec();
    drop(data);
    match String::from_utf8(bytes) {
        Ok(mut text) => Released::Password(SecretText::take(&mut text)),
        Err(error) => {
            let mut bytes = error.into_bytes();
            wipe(&mut bytes);
            Released::Failed("the kept password is not text".to_owned())
        }
    }
}

/// Deletes the items under the service whose account is not `keep`. An item
/// labelled with another identifier is never used, so it is removed
/// (docs/DECISIONS.md D45 item 4).
fn purge_others(keep: &str) {
    // SAFETY: Security's own constants, read as they are.
    let query = unsafe {
        dictionary(vec![
            (kSecClass, constant(kSecClassGenericPassword)),
            (kSecAttrService, text(SERVICE)),
            (kSecMatchLimit, constant(kSecMatchLimitAll)),
            (kSecReturnAttributes, yes()),
        ])
    };
    let mut found: *const c_void = ptr::null();
    // SAFETY: a valid query, and `found` is a valid out pointer. On success the
    // result is a +1 array of attribute dictionaries, which `Retained` takes.
    let status = unsafe { SecItemCopyMatching(as_cf(&query), &mut found) };
    drop(query);
    if status == ITEM_NOT_FOUND {
        return;
    }
    if status != SUCCESS {
        eprintln!("TAWARA unlock: the items could not be listed: {status}");
        return;
    }
    // SAFETY: the query asked for attributes of every match, so the result is an
    // NSArray of NSDictionary.
    let Some(items) =
        (unsafe { Retained::<NSArray<Dictionary>>::from_raw(found.cast_mut().cast()) })
    else {
        return;
    };
    for item in items.iter() {
        // SAFETY: Security's own constant, read as it is.
        let Some(name) = (unsafe { item.get(kSecAttrAccount) }).and_then(text_of) else {
            continue;
        };
        if name == keep {
            continue;
        }
        // SAFETY: Security's own constants, read as they are.
        let query = unsafe {
            dictionary(vec![
                (kSecClass, constant(kSecClassGenericPassword)),
                (kSecAttrService, text(SERVICE)),
                (kSecAttrAccount, text(&name)),
            ])
        };
        // SAFETY: a valid query.
        let status = unsafe { SecItemDelete(as_cf(&query)) };
        if status == SUCCESS || status == ITEM_NOT_FOUND {
            eprintln!("TAWARA unlock: an item of another store was deleted");
        } else {
            eprintln!("TAWARA unlock: an item of another store was not deleted: {status}");
        }
    }
}

/// The access control every item carries: the device's own check, with the
/// item readable only while the device has a passcode and on this device only
/// (docs/DECISIONS.md D45 item 2).
fn access_control() -> Result<Retained<AnyObject>, String> {
    let mut error: *const c_void = ptr::null();
    // SAFETY: Security's own constant, read as it is, and the default allocator
    // (null). `error` is a valid out pointer.
    let control = unsafe {
        SecAccessControlCreateWithFlags(
            ptr::null(),
            (kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly as *const NSString).cast(),
            USER_PRESENCE,
            &mut error,
        )
    };
    if control.is_null() {
        // SAFETY: a failed call returns an owned CFError in `error`, if any.
        let why = unsafe { Retained::<NSError>::from_raw(error.cast_mut().cast()) }.map_or_else(
            || "the system gave no reason".to_owned(),
            |error| error.localizedDescription().to_string(),
        );
        return Err(format!("the access control could not be made: {why}"));
    }
    // SAFETY: a successful call returns an owned access control, which
    // `Retained` takes over.
    unsafe { Retained::from_raw(control.cast_mut().cast()) }
        .ok_or_else(|| "the access control could not be made".to_owned())
}

/// The store identifier as an item's account: its 32 bytes in lower-case hex.
/// The identifier is not secret (docs/DECISIONS.md D45 item 4).
fn account(identifier: &[u8; 32]) -> String {
    identifier
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A dictionary of Keychain attributes, from pairs of a Security key and its
/// value.
fn dictionary(entries: Vec<(&NSString, Retained<AnyObject>)>) -> Retained<Dictionary> {
    let (keys, values): (Vec<&NSString>, Vec<Retained<AnyObject>>) = entries.into_iter().unzip();
    NSDictionary::from_vec(keys.as_slice(), values)
}

/// The dictionary as the `CFDictionaryRef` Security takes. It is valid while the
/// dictionary is.
fn as_cf(dictionary: &Dictionary) -> *const c_void {
    (dictionary as *const Dictionary).cast()
}

/// Any Objective-C object as an `AnyObject`, for a dictionary of mixed values.
fn any<T: Message>(object: Retained<T>) -> Retained<AnyObject> {
    // SAFETY: every Objective-C object is an `AnyObject`, and the cast changes
    // nothing but the static type.
    unsafe { Retained::cast(object) }
}

/// A text value for an attribute.
fn text(value: &str) -> Retained<AnyObject> {
    any(NSString::from_str(value))
}

/// The boolean `true` for an attribute such as `kSecReturnData`. On Apple's
/// platforms this is the same object as `kCFBooleanTrue`.
fn yes() -> Retained<AnyObject> {
    any(NSNumber::new_bool(true))
}

/// A Security constant, as a value in a dictionary.
fn constant(name: &NSString) -> Retained<AnyObject> {
    any(name.copy())
}

/// The text of an attribute value, when it is text.
fn text_of(value: &AnyObject) -> Option<String> {
    // SAFETY: `isKindOfClass:` is sent to an object of any class.
    let is_text: bool = unsafe { msg_send![value, isKindOfClass: NSString::class()] };
    // SAFETY: the value is an NSString, as the check above found, so reading it
    // as one is valid.
    is_text.then(|| unsafe { &*(value as *const AnyObject).cast::<NSString>() }.to_string())
}

/// What a Security status says, in the platform's words, with its code.
fn describe(status: OSStatus) -> String {
    // SAFETY: Security returns an owned string, or null, which is not taken.
    let words = unsafe { SecCopyErrorMessageString(status, ptr::null_mut()) };
    // SAFETY: a non-null result is an owned CFString, which `Retained` takes over.
    match unsafe { Retained::<NSString>::from_raw(words.cast_mut().cast()) } {
        Some(words) => format!("{words} (Keychain error {status})"),
        None => format!("the Keychain failed with error {status}"),
    }
}

/// The name of a release's answer, for the log. The password is never named.
fn kind(released: &Released) -> &'static str {
    match released {
        Released::Password(_) => "a password",
        Released::Declined => "declined",
        Released::Missing => "nothing kept",
        Released::Failed(_) => "failed",
    }
}

/// Overwrites `bytes` with zeros, in writes the compiler may not drop.
fn wipe(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        // SAFETY: `byte` is a valid, aligned and exclusive reference to a `u8`.
        unsafe { ptr::write_volatile(byte, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

/// The one release's answer, which is taken once: by the thread that asks, or
/// by the fallback when no thread could be started.
#[derive(Clone)]
struct Pending(Arc<Mutex<Option<Deliver>>>);

impl Pending {
    fn new(answer: Deliver) -> Pending {
        Pending(Arc::new(Mutex::new(Some(answer))))
    }

    /// Gives `released` to the interface, unless it has been given already.
    fn finish(&self, released: Released) {
        let answer = self.0.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(answer) = answer {
            answer(released);
        }
    }
}

/// The thread's share of a [`Pending`]: when the thread ends without an answer,
/// as a panic makes it, the answer is `Failed`.
struct Answering(Pending);

impl Drop for Answering {
    fn drop(&mut self) {
        self.0.finish(Released::Failed(
            "the check ended without an answer".to_owned(),
        ));
    }
}

/// The release in progress: one at a time, cleared when it ends.
struct Asking;

impl Asking {
    /// Takes the release, or `None` when one is already in progress.
    fn start() -> Option<Asking> {
        (!ASKING.swap(true, Ordering::SeqCst)).then_some(Asking)
    }
}

impl Drop for Asking {
    fn drop(&mut self) {
        ASKING.store(false, Ordering::SeqCst);
    }
}
