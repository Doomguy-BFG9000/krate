//! Signing in through the system's sheet on a Mac (IC-923):
//! ASWebAuthenticationSession, the same class as on an iPhone, risen over
//! the app's window.
//!
//! A Mac app runs on the main thread, so the wait cannot simply block: the
//! sheet needs the main thread to draw and to hear the person. While it is
//! up this turns the application's event loop itself, the way a modal
//! panel does, until the session answers.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol};
use objc2::{define_class, msg_send, ClassType, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::NSApplication;
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

#[link(name = "AuthenticationServices", kind = "framework")]
unsafe extern "C" {}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements for a presentation
    //   context provider.
    // - AuthenticationServices asks it on the main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct KrateMacSignInAnchor;

    // SAFETY: NSObjectProtocol has no additional requirements.
    unsafe impl NSObjectProtocol for KrateMacSignInAnchor {}

    // ASWebAuthenticationPresentationContextProviding, by selector.
    impl KrateMacSignInAnchor {
        /// The window the sheet rises over: the app's own, in front.
        #[unsafe(method_id(presentationAnchorForWebAuthenticationSession:))]
        fn anchor(&self, _session: &AnyObject) -> Option<Retained<AnyObject>> {
            let app = NSApplication::sharedApplication(self.mtm());
            // SAFETY: NSApplication's key window, else its first window.
            unsafe {
                let key: Option<Retained<AnyObject>> = msg_send![&*app, keyWindow];
                key.or_else(|| {
                    let windows: Retained<AnyObject> = msg_send![&*app, windows];
                    msg_send![&*windows, firstObject]
                })
            }
        }
    }
);

type Answer = Result<Option<String>, String>;

thread_local! {
    /// The sheet that is up and its anchor, kept alive until it answers.
    static UP: RefCell<Option<(Retained<AnyObject>, Retained<KrateMacSignInAnchor>)>> =
        const { RefCell::new(None) };
}

/// Open the sheet at `url`; the address the service sends the person back
/// to under `scheme:`, or None when they closed it.
pub(crate) fn sign_in(url: &str, scheme: &str) -> Answer {
    let Some(mtm) = MainThreadMarker::new() else {
        // An app on another thread: open it on the main thread and wait.
        let (url, scheme) = (url.to_string(), scheme.to_string());
        let (tx, rx) = std::sync::mpsc::channel();
        dispatch2::DispatchQueue::main().exec_async(move || {
            // SAFETY: the main queue runs on the main thread.
            let mtm = unsafe { MainThreadMarker::new_unchecked() };
            let slot = Arc::new(Mutex::new(None));
            if let Err(why) = start(mtm, &url, &scheme, slot.clone(), Some(tx.clone())) {
                let _ = tx.send(Err(why));
            }
        });
        return rx
            .recv_timeout(std::time::Duration::from_secs(900))
            .unwrap_or(Ok(None));
    };
    let slot: Arc<Mutex<Option<Answer>>> = Arc::new(Mutex::new(None));
    start(mtm, url, scheme, slot.clone(), None)?;
    let app = NSApplication::sharedApplication(mtm);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
    loop {
        if let Some(answer) = slot.lock().expect("sign-in slot").take() {
            return answer;
        }
        if std::time::Instant::now() > deadline {
            return Ok(None);
        }
        // One turn of the application's own loop, waiting up to 50 ms for
        // an event: the sheet draws, the person types, the answer arrives
        // on the main queue.
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
}

/// Start a session; its answer goes in `slot`, and also down `tx` when an
/// app on another thread is waiting.
fn start(
    mtm: MainThreadMarker,
    url: &str,
    scheme: &str,
    slot: Arc<Mutex<Option<Answer>>>,
    tx: Option<std::sync::mpsc::Sender<Answer>>,
) -> Result<(), String> {
    if UP.with(|up| up.borrow().is_some()) {
        return Err("a sign-in sheet is already open".to_string());
    }
    let (Some(class), Some(url_class)) = (
        AnyClass::get(c"ASWebAuthenticationSession"),
        AnyClass::get(c"NSURL"),
    ) else {
        return Err("this Mac has no sign-in sheet".to_string());
    };
    // SAFETY: an ASWebAuthenticationSession over an NSURL with a completion
    // block that records the answer, the app's window as its anchor,
    // started on the main thread, and kept alive in UP until it answers.
    unsafe {
        let link: Option<Retained<AnyObject>> =
            msg_send![url_class, URLWithString: &*NSString::from_str(url)];
        let link = link.ok_or("the sign-in address could not be read")?;
        let tx = Mutex::new(tx);
        let done = block2::RcBlock::new(move |back: *mut AnyObject, error: *mut AnyObject| {
            let answer = if let Some(back) = back.as_ref() {
                let text: Retained<NSString> = msg_send![back, absoluteString];
                Ok(Some(text.to_string()))
            } else if let Some(error) = error.as_ref() {
                // ASWebAuthenticationSessionErrorCodeCanceledLogin.
                let code: isize = msg_send![error, code];
                if code == 1 {
                    Ok(None)
                } else {
                    let why: Retained<NSString> = msg_send![error, localizedDescription];
                    Err(why.to_string())
                }
            } else {
                Ok(None)
            };
            if let Some(tx) = tx.lock().expect("sign-in lock").take() {
                let _ = tx.send(answer.clone());
            }
            *slot.lock().expect("sign-in slot") = Some(answer);
            UP.with(|up| up.borrow_mut().take());
        });
        let made: objc2::rc::Allocated<AnyObject> = msg_send![class, alloc];
        let session: Option<Retained<AnyObject>> = msg_send![
            made,
            initWithURL: &*link,
            callbackURLScheme: &*NSString::from_str(scheme),
            completionHandler: &*done
        ];
        let session = session.ok_or("the sign-in sheet would not open")?;
        let anchor = KrateMacSignInAnchor::alloc(mtm).set_ivars(());
        let anchor: Retained<KrateMacSignInAnchor> = msg_send![super(anchor), init];
        if let Some(protocol) = AnyProtocol::get(c"ASWebAuthenticationPresentationContextProviding")
        {
            objc2::ffi::class_addProtocol(
                KrateMacSignInAnchor::class() as *const AnyClass as *mut AnyClass,
                protocol,
            );
        }
        let _: () = msg_send![&*session, setPresentationContextProvider: &*anchor];
        UP.with(|up| *up.borrow_mut() = Some((session.clone(), anchor)));
        let started: bool = msg_send![&*session, start];
        if !started {
            UP.with(|up| up.borrow_mut().take());
            return Err("the sign-in sheet would not open".to_string());
        }
    }
    Ok(())
}
