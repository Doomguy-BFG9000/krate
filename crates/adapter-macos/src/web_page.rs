//! A web page inside an app on a Mac (IC-927): WKWebView in the app's
//! window, at the box the app laid out, every navigation judged against the
//! app's grants (`net::web_page_allowed`), with a non-persistent data store
//! and no script channel to the app.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol};
use objc2::{define_class, msg_send, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use krate_adapter_common::ui::WebPageState;

#[link(name = "WebKit", kind = "framework")]
unsafe extern "C" {}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements for a navigation delegate.
    // - WebKit calls it on the main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Cell<u64>]
    struct KrateMacWebDelegate;

    // SAFETY: NSObjectProtocol has no additional requirements.
    unsafe impl NSObjectProtocol for KrateMacWebDelegate {}

    // WKNavigationDelegate, by selector.
    impl KrateMacWebDelegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide(
            &self,
            _web: &AnyObject,
            action: &AnyObject,
            decide: &block2::DynBlock<dyn Fn(isize)>,
        ) {
            let page = self.ivars().get();
            // SAFETY: the action's request and its URL's string.
            let address: String = unsafe {
                let request: Option<Retained<AnyObject>> = msg_send![action, request];
                let url: Option<Retained<AnyObject>> = request.and_then(|r| msg_send![&*r, URL]);
                url.and_then(|u| {
                    let text: Option<Retained<NSString>> = msg_send![&*u, absoluteString];
                    text.map(|t| t.to_string())
                })
                .unwrap_or_default()
            };
            let verdict = PAGES.with(|pages| {
                pages.borrow().get(&page).map(|p| {
                    if address == "about:blank" {
                        Ok(())
                    } else {
                        krate_adapter_common::net::web_page_allowed(&address, &p.granted)
                    }
                })
            });
            match verdict {
                Some(Ok(())) => decide.call((1,)),
                Some(Err(why)) => {
                    PAGES.with(|pages| {
                        if let Some(p) = pages.borrow_mut().get_mut(&page) {
                            p.problem = why;
                        }
                    });
                    decide.call((0,));
                }
                None => decide.call((0,)),
            }
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn failed_early(&self, _web: &AnyObject, _navigation: &AnyObject, error: &AnyObject) {
            failed(self.ivars().get(), error);
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn failed_late(&self, _web: &AnyObject, _navigation: &AnyObject, error: &AnyObject) {
            failed(self.ivars().get(), error);
        }

        #[unsafe(method(webView:didCommitNavigation:))]
        fn committed(&self, _web: &AnyObject, _navigation: &AnyObject) {
            PAGES.with(|pages| {
                if let Some(p) = pages.borrow_mut().get_mut(&self.ivars().get()) {
                    p.problem.clear();
                }
            });
        }
    }
);

struct Page {
    view: Retained<AnyObject>,
    _delegate: Retained<KrateMacWebDelegate>,
    window: Retained<AnyObject>,
    granted: Vec<(String, String)>,
    problem: String,
    /// The box as the app gave it, and the view the page sits in.
    frame: (f32, f32, f32, f32),
    host: Option<Retained<AnyObject>>,
}

thread_local! {
    static PAGES: RefCell<BTreeMap<u64, Page>> = const { RefCell::new(BTreeMap::new()) };
}

static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn failed(page: u64, error: &AnyObject) {
    // SAFETY: an NSError's code and localizedDescription.
    let (code, why): (isize, String) = unsafe {
        let code: isize = msg_send![error, code];
        let why: Retained<NSString> = msg_send![error, localizedDescription];
        (code, why.to_string())
    };
    if code == -999 || code == 102 {
        return;
    }
    PAGES.with(|pages| {
        if let Some(p) = pages.borrow_mut().get_mut(&page) {
            p.problem = why;
        }
    });
}

/// Run `f` on the main thread: at once when already there (a Mac app runs
/// on it), else dispatched and waited for.
fn on_main<T: Send + 'static>(f: impl FnOnce(MainThreadMarker) -> T + Send + 'static) -> Option<T> {
    if let Some(mtm) = MainThreadMarker::new() {
        return Some(f(mtm));
    }
    let (tx, rx) = std::sync::mpsc::channel();
    dispatch2::DispatchQueue::main().exec_async(move || {
        // SAFETY: the main queue runs on the main thread.
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        let _ = tx.send(f(mtm));
    });
    rx.recv_timeout(std::time::Duration::from_secs(5)).ok()
}

/// The box, in the window's content view.
fn frame_in(window: &AnyObject, frame: (f32, f32, f32, f32)) -> NSRect {
    // SAFETY: the window's content view.
    let content: Option<Retained<AnyObject>> = unsafe { msg_send![window, contentView] };
    match content {
        Some(content) => frame_in_view(&content, frame),
        None => NSRect::new(
            NSPoint::new(frame.0 as f64, frame.1 as f64),
            NSSize::new(frame.2 as f64, frame.3 as f64),
        ),
    }
}

/// The box in `view`: AppKit measures from the bottom unless the view is
/// flipped; the app measures from the top.
fn frame_in_view(view: &AnyObject, frame: (f32, f32, f32, f32)) -> NSRect {
    // SAFETY: the view's bounds and isFlipped.
    let (height, flipped) = unsafe {
        let bounds: NSRect = msg_send![view, bounds];
        let flipped: bool = msg_send![view, isFlipped];
        (bounds.size.height, flipped)
    };
    let (x, y, w, h) = (
        frame.0 as f64,
        frame.1 as f64,
        frame.2 as f64,
        frame.3 as f64,
    );
    let y = if flipped { y } else { height - y - h };
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn load(view: &AnyObject, address: &str) {
    // SAFETY: an NSURLRequest for the address, loaded by the web view.
    unsafe {
        let (Some(url_class), Some(request_class)) =
            (AnyClass::get(c"NSURL"), AnyClass::get(c"NSURLRequest"))
        else {
            return;
        };
        let url: Option<Retained<AnyObject>> =
            msg_send![url_class, URLWithString: &*NSString::from_str(address)];
        let Some(url) = url else {
            return;
        };
        let request: Retained<AnyObject> = msg_send![request_class, requestWithURL: &*url];
        let _: Option<Retained<AnyObject>> = msg_send![view, loadRequest: &*request];
    }
}

/// Show a page in the window whose NSWindow is `ns_window`.
pub(crate) fn open(
    ns_window: u64,
    address: &str,
    frame: (f32, f32, f32, f32),
    granted: Vec<(String, String)>,
) -> Result<u64, String> {
    let page = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let address = address.to_string();
    on_main(move |mtm| -> Result<u64, String> {
        let (Some(config_class), Some(store_class), Some(web_class)) = (
            AnyClass::get(c"WKWebViewConfiguration"),
            AnyClass::get(c"WKWebsiteDataStore"),
            AnyClass::get(c"WKWebView"),
        ) else {
            return Err("this Mac has no web view".to_string());
        };
        // SAFETY: the NSWindow the adapter made for this app (its pointer is
        // the native handle), retained here; a WKWebView with a
        // non-persistent data store and Krate's delegate added to its
        // content view, on the main thread.
        unsafe {
            let window: Retained<AnyObject> =
                Retained::retain(ns_window as usize as *mut AnyObject)
                    .ok_or("the app's window is gone")?;
            let config: Retained<AnyObject> = msg_send![config_class, new];
            let store: Retained<AnyObject> = msg_send![store_class, nonPersistentDataStore];
            let _: () = msg_send![&*config, setWebsiteDataStore: &*store];
            let made: objc2::rc::Allocated<AnyObject> = msg_send![web_class, alloc];
            let view: Retained<AnyObject> =
                msg_send![made, initWithFrame: frame_in(&window, frame), configuration: &*config];
            let delegate = KrateMacWebDelegate::alloc(mtm).set_ivars(Cell::new(page));
            let delegate: Retained<KrateMacWebDelegate> = msg_send![super(delegate), init];
            if let Some(protocol) = AnyProtocol::get(c"WKNavigationDelegate") {
                objc2::ffi::class_addProtocol(
                    KrateMacWebDelegate::class() as *const AnyClass as *mut AnyClass,
                    protocol,
                );
            }
            let _: () = msg_send![&*view, setNavigationDelegate: &*delegate];
            let content: Option<Retained<AnyObject>> = msg_send![&*window, contentView];
            let content = content.ok_or("the app's window has no content")?;
            let _: () = msg_send![&*content, addSubview: &*view];
            load(&view, &address);
            PAGES.with(|pages| {
                pages.borrow_mut().insert(
                    page,
                    Page {
                        view,
                        _delegate: delegate,
                        window,
                        granted,
                        problem: String::new(),
                        frame,
                        host: None,
                    },
                )
            });
        }
        Ok(page)
    })
    .ok_or_else(|| "the web view did not answer".to_string())?
}

/// Keep the page above the app's own drawing. The window's canvas view is
/// added over the content once the first frame is drawn, and moving the
/// page back in front of it does not hold; inside the canvas view, as its
/// child, the page always draws over the canvas.
fn keep_on_top(p: &mut Page) {
    // SAFETY: the content view's subviews and the page's superview; moving
    // a view into another removes it from its old superview.
    unsafe {
        let content: Option<Retained<AnyObject>> = msg_send![&*p.window, contentView];
        let Some(content) = content else {
            return;
        };
        let subviews: Retained<AnyObject> = msg_send![&*content, subviews];
        let count: usize = msg_send![&*subviews, count];
        let mut host: Retained<AnyObject> = content.clone();
        for i in (0..count).rev() {
            let view: Retained<AnyObject> = msg_send![&*subviews, objectAtIndex: i];
            if Retained::as_ptr(&view) != Retained::as_ptr(&p.view) {
                host = view;
                break;
            }
        }
        let parent: Option<Retained<AnyObject>> = msg_send![&*p.view, superview];
        let inside = parent.is_some_and(|s| Retained::as_ptr(&s) == Retained::as_ptr(&host));
        if !inside {
            let _: () = msg_send![&*host, addSubview: &*p.view];
            let _: () = msg_send![&*p.view, setFrame: frame_in_view(&host, p.frame)];
            if std::env::var_os("KRATE_WEB_TRACE").is_some() {
                let class: Retained<NSString> = msg_send![&*host, className];
                eprintln!("web: moved into {class}");
            }
        }
        p.host = Some(host);
    }
}

fn with_page(page: u64, f: impl FnOnce(&Page) + Send + 'static) {
    let _ = on_main(move |_| {
        PAGES.with(|pages| {
            if let Some(p) = pages.borrow().get(&page) {
                f(p);
            }
        })
    });
}

pub(crate) fn place(page: u64, frame: (f32, f32, f32, f32)) {
    let _ = on_main(move |_| {
        PAGES.with(|pages| {
            if let Some(p) = pages.borrow_mut().get_mut(&page) {
                p.frame = frame;
                let at = match &p.host {
                    Some(host) => frame_in_view(host, frame),
                    None => frame_in(&p.window, frame),
                };
                // SAFETY: a WKWebView's frame, on the main thread.
                unsafe {
                    let _: () = msg_send![&*p.view, setFrame: at];
                }
            }
        })
    });
}

pub(crate) fn go(page: u64, address: &str) {
    let address = address.to_string();
    with_page(page, move |p| load(&p.view, &address));
}

pub(crate) fn back(page: u64) {
    with_page(page, |p| {
        // SAFETY: WKWebView's goBack, on the main thread.
        unsafe {
            let _: Option<Retained<AnyObject>> = msg_send![&*p.view, goBack];
        }
    });
}

pub(crate) fn state(page: u64) -> Option<WebPageState> {
    on_main(move |_| {
        PAGES.with(|pages| {
            pages.borrow_mut().get_mut(&page).map(|p| {
                keep_on_top(p);
                // SAFETY: WKWebView's URL, title and isLoading, main thread.
                unsafe {
                    let url: Option<Retained<AnyObject>> = msg_send![&*p.view, URL];
                    let address = url
                        .and_then(|u| {
                            let t: Option<Retained<NSString>> = msg_send![&*u, absoluteString];
                            t.map(|t| t.to_string())
                        })
                        .unwrap_or_default();
                    let title: Option<Retained<NSString>> = msg_send![&*p.view, title];
                    let loading: bool = msg_send![&*p.view, isLoading];
                    WebPageState {
                        address,
                        title: title.map(|t| t.to_string()).unwrap_or_default(),
                        loading,
                        problem: p.problem.clone(),
                    }
                }
            })
        })
    })
    .flatten()
}

pub(crate) fn close(page: u64) {
    let _ = on_main(move |_| {
        if let Some(p) = PAGES.with(|pages| pages.borrow_mut().remove(&page)) {
            // SAFETY: removing a WKWebView from its superview, main thread.
            unsafe {
                let _: () = msg_send![&*p.view, removeFromSuperview];
            }
        }
    });
}
