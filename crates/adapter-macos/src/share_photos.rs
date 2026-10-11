//! The share sheet and adding to Photos on a Mac (IC-919, IC-920), the
//! halves a phone already had. Both wait for the person, and a Mac app runs
//! on the main thread, so the wait turns the application's own event loop
//! -- the picker draws, the Photos prompt answers -- as a modal panel does.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, Bool};
use objc2::{define_class, msg_send, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::NSApplication;
use objc2_foundation::{NSArray, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

#[link(name = "Photos", kind = "framework")]
unsafe extern "C" {}

/// One turn of the application's loop, waiting up to 50 ms for an event.
fn turn(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    autoreleasepool(|_| {
        // SAFETY: on the main thread; NSDate and the event call as AppKit
        // documents them.
        unsafe {
            let Some(date_class) = AnyClass::get(c"NSDate") else {
                return;
            };
            let until: Retained<AnyObject> =
                msg_send![date_class, dateWithTimeIntervalSinceNow: 0.05f64];
            let mode = NSString::from_str("kCFRunLoopDefaultMode");
            let event: Option<Retained<AnyObject>> = msg_send![
                &*app,
                nextEventMatchingMask: u64::MAX,
                untilDate: &*until,
                inMode: &*mode,
                dequeue: true
            ];
            if let Some(event) = event {
                let _: () = msg_send![&*app, sendEvent: &*event];
            }
        }
    });
}

/// Wait for an answer on `rx`, turning the app's loop when on the main
/// thread so what is being waited for can happen.
fn wait<T>(rx: &Receiver<T>, most: Duration) -> Option<T> {
    let deadline = Instant::now() + most;
    match MainThreadMarker::new() {
        Some(mtm) => loop {
            match rx.try_recv() {
                Ok(value) => return Some(value),
                Err(TryRecvError::Disconnected) => return None,
                Err(TryRecvError::Empty) => {}
            }
            if Instant::now() > deadline {
                return None;
            }
            turn(mtm);
        },
        None => rx.recv_timeout(most).ok(),
    }
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements for a picker delegate.
    // - AppKit calls it on the main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = std::sync::Mutex<Option<std::sync::mpsc::Sender<bool>>>]
    struct KrateSharePickerDelegate;

    // SAFETY: NSObjectProtocol has no additional requirements.
    unsafe impl NSObjectProtocol for KrateSharePickerDelegate {}

    impl KrateSharePickerDelegate {
        /// The person picked a way to share (or closed the picker: nil).
        #[unsafe(method(sharingServicePicker:didChooseSharingService:))]
        fn chose(&self, _picker: &AnyObject, service: *mut AnyObject) {
            if let Some(tx) = self.ivars().lock().expect("share lock").take() {
                let _ = tx.send(!service.is_null());
            }
        }
    }
);

/// The Mac's share picker with a text, a link and files, from the middle of
/// the app's window; true when the person chose where to send it.
pub(crate) fn share(
    ns_window: u64,
    text: Option<&str>,
    url: Option<&str>,
    files: &[(String, Vec<u8>)],
) -> Result<bool, String> {
    let mtm = MainThreadMarker::new().ok_or("the share picker opens from the main thread")?;
    let dir = std::env::temp_dir()
        .join("krate-share")
        .join(format!("{}", std::process::id()));
    let mut paths = Vec::new();
    if !files.is_empty() {
        std::fs::create_dir_all(&dir).map_err(|e| format!("could not stage the files: {e}"))?;
        for (name, data) in files {
            let path = dir.join(name);
            std::fs::write(&path, data).map_err(|e| format!("could not stage {name}: {e}"))?;
            paths.push(path);
        }
    }
    let (tx, rx) = std::sync::mpsc::channel();
    // SAFETY: NSString/NSURL items in an NSArray, an NSSharingServicePicker
    // over them with Krate's delegate, shown from the window's content view
    // on the main thread; the delegate is kept alive until it answers.
    let _delegate = unsafe {
        let (Some(picker_class), Some(url_class)) = (
            AnyClass::get(c"NSSharingServicePicker"),
            AnyClass::get(c"NSURL"),
        ) else {
            return Err("this Mac has no share picker".to_string());
        };
        let mut items: Vec<Retained<AnyObject>> = Vec::new();
        if let Some(text) = text.filter(|t| !t.is_empty()) {
            items.push(Retained::cast_unchecked(NSString::from_str(text)));
        }
        if let Some(url) = url.filter(|u| !u.is_empty()) {
            let link: Option<Retained<AnyObject>> =
                msg_send![url_class, URLWithString: &*NSString::from_str(url)];
            if let Some(link) = link {
                items.push(link);
            }
        }
        for path in &paths {
            let file: Option<Retained<AnyObject>> = msg_send![url_class, fileURLWithPath: &*NSString::from_str(&path.to_string_lossy())];
            if let Some(file) = file {
                items.push(file);
            }
        }
        if items.is_empty() {
            return Err("nothing to share".to_string());
        }
        let items = NSArray::from_retained_slice(&items);
        let made: objc2::rc::Allocated<AnyObject> = msg_send![picker_class, alloc];
        let picker: Retained<AnyObject> = msg_send![made, initWithItems: &*items];
        let delegate =
            KrateSharePickerDelegate::alloc(mtm).set_ivars(std::sync::Mutex::new(Some(tx)));
        let delegate: Retained<KrateSharePickerDelegate> = msg_send![super(delegate), init];
        if let Some(protocol) = AnyProtocol::get(c"NSSharingServicePickerDelegate") {
            objc2::ffi::class_addProtocol(
                KrateSharePickerDelegate::class() as *const AnyClass as *mut AnyClass,
                protocol,
            );
        }
        let _: () = msg_send![&*picker, setDelegate: &*delegate];
        let window: Retained<AnyObject> = Retained::retain(ns_window as usize as *mut AnyObject)
            .ok_or("the app's window is gone")?;
        let content: Option<Retained<AnyObject>> = msg_send![&*window, contentView];
        let content = content.ok_or("the app's window has no content")?;
        let bounds: NSRect = msg_send![&*content, bounds];
        let at = NSRect::new(
            NSPoint::new(
                bounds.size.width / 2.0 - 1.0,
                bounds.size.height / 2.0 - 1.0,
            ),
            NSSize::new(2.0, 2.0),
        );
        // NSRectEdgeMinY: the picker opens under the point.
        let _: () =
            msg_send![&*picker, showRelativeToRect: at, ofView: &*content, preferredEdge: 1usize];
        (picker, delegate)
    };
    let answer = wait(&rx, Duration::from_secs(600)).unwrap_or(false);
    if !paths.is_empty() {
        // The service has the files once it is chosen; a moment before
        // removing them lets it copy what it needs.
        let dir = dir.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(60));
            let _ = std::fs::remove_dir_all(dir);
        });
    }
    Ok(answer)
}

/// Add a PNG or JPEG to Photos, add-only: Err(None) when the person said no.
pub(crate) fn save_photo(data: &[u8]) -> Result<(), Option<String>> {
    let (Some(library), Some(image_class), Some(request_class), Some(data_class)) = (
        AnyClass::get(c"PHPhotoLibrary"),
        AnyClass::get(c"NSImage"),
        AnyClass::get(c"PHAssetChangeRequest"),
        AnyClass::get(c"NSData"),
    ) else {
        return Err(Some("this Mac has no Photos library".to_string()));
    };
    // macOS kills a process that asks for Photos without this Info.plist
    // key; say so instead.
    // SAFETY: the main bundle's info dictionary lookup; nil when absent.
    let declared: bool = unsafe {
        let bundle: Retained<AnyObject> = msg_send![objc2::class!(NSBundle), mainBundle];
        let key = NSString::from_str("NSPhotoLibraryAddUsageDescription");
        let value: Option<Retained<AnyObject>> =
            msg_send![&*bundle, objectForInfoDictionaryKey: &*key];
        value.is_some()
    };
    if !declared {
        return Err(Some(
            "this Krate is not an app bundle that declares adding to Photos: open the app from Krate.app"
                .to_string(),
        ));
    }
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), Option<String>>>();
    let bytes = data.to_vec();
    // SAFETY: requestAuthorizationForAccessLevel: add-only (1); on success the
    // picture decoded as an NSImage and an asset made from it in
    // performChanges:, whose completion answers.
    unsafe {
        let tx_auth = tx.clone();
        let asked = block2::RcBlock::new(move |status: isize| {
            // Authorized (3) or limited (4).
            if status != 3 && status != 4 {
                let _ = tx_auth.send(Err(None));
                return;
            }
            let ns_data: Option<Retained<AnyObject>> = msg_send![data_class, dataWithBytes: bytes.as_ptr() as *const std::ffi::c_void, length: bytes.len()];
            let image: Option<Retained<AnyObject>> = ns_data.and_then(|d| {
                let made: objc2::rc::Allocated<AnyObject> = msg_send![image_class, alloc];
                msg_send![made, initWithData: &*d]
            });
            let Some(image) = image else {
                let _ = tx_auth.send(Err(Some("that picture could not be read".to_string())));
                return;
            };
            let change = block2::RcBlock::new(move || {
                let _: Option<Retained<AnyObject>> =
                    msg_send![request_class, creationRequestForAssetFromImage: &*image];
            });
            let tx_done = tx_auth.clone();
            let done = block2::RcBlock::new(move |ok: Bool, error: *mut AnyObject| {
                let _ = tx_done.send(if ok.as_bool() {
                    Ok(())
                } else if error.is_null() {
                    Err(Some("Photos refused the picture".to_string()))
                } else {
                    let text: Option<Retained<NSString>> = msg_send![&*error, localizedDescription];
                    Err(Some(text.map(|t| t.to_string()).unwrap_or_default()))
                });
            });
            let shared: Retained<AnyObject> = msg_send![library, sharedPhotoLibrary];
            let _: () = msg_send![&*shared, performChanges: &*change, completionHandler: &*done];
        });
        let _: () =
            msg_send![library, requestAuthorizationForAccessLevel: 1isize, handler: &*asked];
    }
    wait(&rx, Duration::from_secs(120)).unwrap_or(Err(Some("Photos did not answer".to_string())))
}
