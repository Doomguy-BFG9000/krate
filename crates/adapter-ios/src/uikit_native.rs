//! Real UIKit windowing for the iOS adapter.
//!
//! Two threads, on purpose. The main thread belongs to UIKit: it returns
//! from every delegate callout and UIApplicationMain's own run loop runs
//! free. The guest lives on a second thread, and every UIKit touch (create
//! the window, put a frame on screen) is marshaled to the main queue with
//! a synchronous dispatch. The first design ran the guest inside a
//! delegate callout and pumped a nested run loop -- but a nested run loop
//! never drains the main dispatch queue, iOS delivers parts of its own
//! touch pipeline through that queue, and the result was input that
//! arrived whenever something else happened to jostle the loop. A real
//! iPhone said "very late in response"; this split is the answer.
//!
//! Input flows the other way without blocking anyone: touch callouts on
//! the main thread push raw samples into shared state and signal a
//! condvar; the guest's park waits on that condvar and wakes the instant
//! a finger lands. Rasterization happens on the guest thread -- the main
//! thread only wraps finished pixels in a CGImage and hands them to the
//! view.
//!
//! iOS has one screen-sized window; `create_native_window` creates it on
//! first use and re-dresses it for each sequential guest (the wall sheet,
//! then the app).

#[cfg(target_os = "ios")]
pub use real::*;

#[cfg(not(target_os = "ios"))]
pub use stub::*;

use krate_adapter_common::ui::{
    RawKeySample, RawPointerSample, RawWheelSample, UiAdapterError, WidgetPlacement, WindowId,
    WindowSize, WinitWindowNativeEvent, WinitWindowSnapshot,
};

/// Native events paired with the Krate window they belong to.
pub type CollectedNativeEvents = Vec<(WindowId, WinitWindowNativeEvent)>;

#[cfg(target_os = "ios")]
mod real {
    use super::*;
    use std::cell::RefCell;
    use std::sync::{Condvar, Mutex, OnceLock};

    use dispatch2::DispatchQueue;
    use objc2::rc::Retained;
    use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
    use objc2_core_foundation::{CGPoint, CGRect};
    use objc2_core_graphics::{
        CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpaceCreateDeviceRGB,
        CGImageAlphaInfo, CGImageByteOrderInfo,
    };
    use objc2_foundation::NSObjectProtocol;
    use objc2_ui_kit::{
        UIApplication, UIImage, UIImageView, UIScreen, UITouch, UITouchPhase, UIView,
        UIViewController, UIWindow, UIWindowScene,
    };
    use objc2_ui_kit::{UIKeyInput, UITextInputTraits};

    /// One raw touch, in logical points, straight from the main-thread
    /// callouts. The gesture logic runs on the guest thread.
    #[derive(Clone, Copy)]
    struct RawTouch {
        phase: UITouchPhase,
        x: f32,
        y: f32,
        finger: usize,
    }

    /// State both threads touch, under one lock. Kept deliberately small:
    /// raw input in, screen geometry out.
    /// What the on-screen keyboard typed, in order.
    enum Typed {
        Text(String),
        Backspace,
        Enter,
    }

    struct Shared {
        touches: Vec<RawTouch>,
        /// Typing from the on-screen keyboard, waiting for the guest.
        typed: Vec<Typed>,
        /// The close capsule was tapped: the app is asked to close.
        close_tapped: bool,
        /// Set once the main thread has built the window.
        screen: Option<(WindowSize, f32)>,
        /// The CAMetalLayer's raw pointer plus the FULL native scale --
        /// the GPU renders at true density, so the CPU path's 2x quality
        /// cap dies here.
        metal_layer: Option<(usize, f32)>,
        /// The panel's own scale (3 on a current iPhone): whole-frame
        /// pictures are drawn at it, one pass, with no second stretch by the
        /// compositor.
        panel_scale: f32,
    }

    static SHARED: Mutex<Shared> = Mutex::new(Shared {
        touches: Vec::new(),
        typed: Vec::new(),
        close_tapped: false,
        screen: None,
        metal_layer: None,
        panel_scale: 2.0,
    });

    /// Signaled by the touch callouts AND by the display link, so a parked
    /// guest wakes on input or on vsync, whichever comes first.
    static INPUT_ARRIVED: Condvar = Condvar::new();

    /// Set by the display link each time the panel is about to refresh, and
    /// cleared by the guest when it wakes. This is what replaces sleeping a
    /// fixed 16 ms and hoping it lines up with the display: on a 120 Hz
    /// iPhone the panel decides when a frame is due, so frames land on the
    /// refresh instead of drifting against it (K-090's last structural
    /// cause).
    static VSYNC_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// TEMP on-device diagnostics for K-090: a timestamped log in the app
    /// container, pulled over USB. Nothing else escapes a real iPhone.
    pub fn diag(line: &str) {
        use std::io::Write;
        // Opt-in: only with Documents/krate-diag.on present. Writing a line
        // per frame, forever, cost every frame a file write and grew the
        // log without end.
        static ON: OnceLock<bool> = OnceLock::new();
        let on = *ON.get_or_init(|| {
            std::env::var("HOME")
                .map(|h| {
                    std::path::Path::new(&h)
                        .join("Documents/krate-diag.on")
                        .exists()
                })
                .unwrap_or(false)
        });
        if !on {
            return;
        }
        static FILE: OnceLock<Mutex<std::fs::File>> = OnceLock::new();
        static EPOCH: OnceLock<std::time::Instant> = OnceLock::new();
        let epoch = EPOCH.get_or_init(std::time::Instant::now);
        let file = FILE.get_or_init(|| {
            let path = std::env::var("HOME")
                .map(|h| std::path::PathBuf::from(h).join("Documents/krate-diag.log"))
                .unwrap_or_else(|_| "/tmp/krate-diag.log".into());
            let _ = std::fs::create_dir_all(path.parent().unwrap());
            Mutex::new(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .expect("diag log"),
            )
        });
        let ms = epoch.elapsed().as_secs_f64() * 1000.0;
        let mut file = file.lock().expect("diag lock");
        let _ = writeln!(file, "{ms:.1} {line}");
        let _ = file.flush();
    }

    /// Guest-thread state: gesture synthesis, samples, and the raster
    /// buffer. Only the guest thread touches it.
    struct GuestSide {
        krate: Option<WindowId>,
        logical: WindowSize,
        scale: f32,
        placements: Vec<WidgetPlacement>,
        events: CollectedNativeEvents,
        pointer_samples: Vec<RawPointerSample>,
        key_samples: Vec<RawKeySample>,
        wheel_samples: Vec<RawWheelSample>,
        /// Scroll deltas accumulate here between drains: an iPhone reports
        /// touches at 120 Hz, and queueing one wheel event per report made
        /// the app replay a two-second backlog after the finger stopped
        /// (one frame per event, 16 ms floor). One coalesced delta per
        /// drain is what a finger actually means.
        pending_scroll: Option<(f32, f32, f32, f32)>,
        gesture: Option<TouchGesture>,
        /// Every finger, for apps that read them all (krate:ui/touch).
        fingers: krate_adapter_common::ui::FingerTracker,
        hovered: Option<krate_adapter_common::ui::WidgetId>,
        pressed_widget: Option<krate_adapter_common::ui::WidgetId>,
        paint_buffer: Vec<u32>,
        dirty: bool,
    }

    struct TouchGesture {
        finger: usize,
        start: (f32, f32),
        last: (f32, f32),
        scrolling: bool,
        /// When the finger landed, and whether its press has gone out yet.
        began: std::time::Instant,
        pressed: bool,
    }

    /// How long a finger rests before its press goes out, unless it starts
    /// to scroll first -- iOS's own rule for touches inside scrolling
    /// content. A tap used to press only when the finger lifted, a beat
    /// late on every button; a drag that scrolls still presses nothing.
    const PRESS_AFTER: std::time::Duration = std::time::Duration::from_millis(50);

    thread_local! {
        static GUEST: RefCell<Option<GuestSide>> = const { RefCell::new(None) };
    }

    /// Main-thread-only UIKit objects, created on first use via dispatch.
    struct MainSide {
        window: Retained<UIWindow>,
        /// Krate's close capsule, over every app (see `set_app_chrome`).
        close: Retained<objc2_ui_kit::UIButton>,
        /// Krate's loading cover (see `show_loading`).
        loading: Loading,
        _close_target: Retained<KrateCloseTarget>,
        controller: Retained<KrateViewController>,
        metal: Retained<objc2_quartz_core::CAMetalLayer>,
        /// Held so the link's target outlives the link that calls it.
        _link_target: Retained<KrateDisplayLinkTarget>,
        _display_link: Retained<objc2_quartz_core::CADisplayLink>,
        view: Retained<KrateSurfaceView>,
    }

    struct MainSideCell(OnceLock<MainSide>);
    // SAFETY: the cell is written and read only on the main thread, inside
    // main-queue closures; it only crosses threads as an opaque pointer.
    unsafe impl Send for MainSideCell {}
    unsafe impl Sync for MainSideCell {}
    static MAIN_SIDE: MainSideCell = MainSideCell(OnceLock::new());

    define_class!(
        // SAFETY:
        // - NSObject has no subclassing requirements for a target-action
        //   receiver.
        // - CADisplayLink calls this on the main thread, which is where it
        //   was added to the run loop.
        #[unsafe(super = objc2_foundation::NSObject)]
        #[thread_kind = MainThreadOnly]
        #[ivars = ()]
        struct KrateDisplayLinkTarget;

        // SAFETY: NSObjectProtocol has no additional requirements.
        unsafe impl NSObjectProtocol for KrateDisplayLinkTarget {}

        impl KrateDisplayLinkTarget {
            // SAFETY: matches CADisplayLink's target-action signature.
            #[unsafe(method(krateDisplayLinkFired:))]
            fn fired(&self, _link: &objc2_quartz_core::CADisplayLink) {
                VSYNC_PENDING.store(true, std::sync::atomic::Ordering::Relaxed);
                diag("vsync");
                // Waking the guest costs nothing when it is already awake,
                // and is the whole point when it is parked.
                INPUT_ARRIVED.notify_all();
            }
        }
    );

    define_class!(
        // SAFETY:
        // - UIImageView has no extra subclassing requirements for touch
        //   overrides.
        // - UIKit delivers touches on the main thread.
        #[unsafe(super = UIImageView)]
        #[thread_kind = MainThreadOnly]
        #[ivars = ()]
        struct KrateSurfaceView;

        // SAFETY: NSObjectProtocol has no additional requirements.
        unsafe impl NSObjectProtocol for KrateSurfaceView {}

        // The view takes the on-screen keyboard's typing when an app asks
        // for text entry (krate:ui/text-entry). No autocorrection: a name in
        // a game, a command in an emulator, is typed as meant.
        unsafe impl UITextInputTraits for KrateSurfaceView {
            #[unsafe(method(autocorrectionType))]
            fn autocorrection_type(&self) -> objc2_ui_kit::UITextAutocorrectionType {
                objc2_ui_kit::UITextAutocorrectionType::No
            }

            #[unsafe(method(autocapitalizationType))]
            fn autocapitalization_type(&self) -> objc2_ui_kit::UITextAutocapitalizationType {
                objc2_ui_kit::UITextAutocapitalizationType::None
            }
        }

        unsafe impl UIKeyInput for KrateSurfaceView {
            #[unsafe(method(hasText))]
            fn has_text(&self) -> bool {
                true
            }

            #[unsafe(method(insertText:))]
            fn insert_text(&self, text: &objc2_foundation::NSString) {
                let text = text.to_string();
                let mut shared = SHARED.lock().expect("shared state lock");
                for part in text.split_inclusive('\n') {
                    match part.strip_suffix('\n') {
                        Some(before) => {
                            if !before.is_empty() {
                                shared.typed.push(Typed::Text(before.to_string()));
                            }
                            shared.typed.push(Typed::Enter);
                        }
                        None => shared.typed.push(Typed::Text(part.to_string())),
                    }
                }
                drop(shared);
                INPUT_ARRIVED.notify_all();
            }

            #[unsafe(method(deleteBackward))]
            fn delete_backward(&self) {
                SHARED.lock().expect("shared state lock").typed.push(Typed::Backspace);
                INPUT_ARRIVED.notify_all();
            }
        }

        impl KrateSurfaceView {
            // Only a first responder gets the keyboard's typing.
            #[unsafe(method(canBecomeFirstResponder))]
            fn can_become_first_responder(&self) -> bool {
                true
            }

            // SAFETY: signature matches UIView's.
            #[unsafe(method(layoutSubviews))]
            fn layout_subviews(&self) {
                let _: () = unsafe { msg_send![super(self), layoutSubviews] };
                view_laid_out(self);
            }

            // SAFETY: signatures match UIResponder's touch methods.
            #[unsafe(method(touchesBegan:withEvent:))]
            fn touches_began(
                &self,
                touches: &objc2_foundation::NSSet<UITouch>,
                _event: Option<&objc2_ui_kit::UIEvent>,
            ) {
                record_touches(self, touches);
            }

            #[unsafe(method(touchesMoved:withEvent:))]
            fn touches_moved(
                &self,
                touches: &objc2_foundation::NSSet<UITouch>,
                _event: Option<&objc2_ui_kit::UIEvent>,
            ) {
                record_touches(self, touches);
            }

            #[unsafe(method(touchesEnded:withEvent:))]
            fn touches_ended(
                &self,
                touches: &objc2_foundation::NSSet<UITouch>,
                _event: Option<&objc2_ui_kit::UIEvent>,
            ) {
                record_touches(self, touches);
            }

            #[unsafe(method(touchesCancelled:withEvent:))]
            fn touches_cancelled(
                &self,
                touches: &objc2_foundation::NSSet<UITouch>,
                _event: Option<&objc2_ui_kit::UIEvent>,
            ) {
                record_touches(self, touches);
            }
        }
    );

    define_class!(
        // SAFETY:
        // - NSObject has no subclassing requirements for a target-action
        //   receiver.
        // - UIKit calls it on the main thread.
        #[unsafe(super = objc2_foundation::NSObject)]
        #[thread_kind = MainThreadOnly]
        #[ivars = ()]
        struct KrateCloseTarget;

        // SAFETY: NSObjectProtocol has no additional requirements.
        unsafe impl NSObjectProtocol for KrateCloseTarget {}

        impl KrateCloseTarget {
            #[unsafe(method(krateClose:))]
            fn krate_close(&self, _sender: &objc2_foundation::NSObject) {
                // The cover at once, so the tap answers before the app has
                // finished closing and Krate's home is drawn.
                LOADING.store(true, std::sync::atomic::Ordering::Relaxed);
                *LOADING_SINCE.lock().expect("loading lock") = Some(std::time::Instant::now());
                if let Some(main_side) = MAIN_SIDE.0.get() {
                    main_side.close.setHidden(true);
                    cover_shown(main_side, "Krate", "");
                }
                SHARED.lock().expect("shared state lock").close_tapped = true;
                INPUT_ARRIVED.notify_all();
            }
        }
    );

    /// Krate's loading cover: the mark, a title, a line of status and the
    /// system spinner, over everything while an app opens or Krate gets
    /// ready -- so a person never watches a blank screen, or an app's first
    /// frame drawn for the wrong size.
    struct Loading {
        cover: Retained<UIView>,
        mark: Retained<UIImageView>,
        title: Retained<objc2_ui_kit::UILabel>,
        status: Retained<objc2_ui_kit::UILabel>,
        spinner: Retained<objc2_ui_kit::UIActivityIndicatorView>,
    }

    static LOADING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static LOADING_SINCE: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    /// The cover lifts on its own after this, whatever the app does.
    const LOADING_MOST: std::time::Duration = std::time::Duration::from_secs(5);

    fn cover_shown(main_side: &MainSide, title: &str, status: &str) {
        let l = &main_side.loading;
        l.title
            .setText(Some(&objc2_foundation::NSString::from_str(title)));
        l.status
            .setText(Some(&objc2_foundation::NSString::from_str(status)));
        l.cover.setHidden(false);
        let host: &UIView = &main_side.view;
        host.bringSubviewToFront(&l.cover);
        l.spinner.startAnimating();
    }

    /// Cover the screen with Krate's loading cover. It lifts by itself on
    /// the first frame an app draws at the screen's real size (or after five
    /// seconds), or with `hide_loading`.
    pub fn show_loading(title: &str, status: &str) {
        if ensure_main_side().is_err() {
            return;
        }
        LOADING.store(true, std::sync::atomic::Ordering::Relaxed);
        *LOADING_SINCE.lock().expect("loading lock") = Some(std::time::Instant::now());
        let (title, status) = (title.to_string(), status.to_string());
        DispatchQueue::main().exec_async(move || {
            if let Some(main_side) = MAIN_SIDE.0.get() {
                cover_shown(main_side, &title, &status);
            }
        });
    }

    /// Lift the loading cover.
    pub fn hide_loading() {
        if !LOADING.swap(false, std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        DispatchQueue::main().exec_async(|| {
            if let Some(main_side) = MAIN_SIDE.0.get() {
                main_side.loading.cover.setHidden(true);
                main_side.loading.spinner.stopAnimating();
            }
        });
    }

    /// A frame of `size` reached the screen: the cover lifts when the frame
    /// is the screen's real size, or when it has been up too long.
    fn frame_shown(size: Option<WindowSize>) {
        if !LOADING.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let screen = SHARED
            .lock()
            .expect("shared state lock")
            .screen
            .map(|(l, _)| l);
        let right_size = match (size, screen) {
            (Some(frame), Some(screen)) => {
                frame.width.abs_diff(screen.width) <= 1 && frame.height.abs_diff(screen.height) <= 1
            }
            _ => true,
        };
        let too_long = LOADING_SINCE
            .lock()
            .expect("loading lock")
            .is_some_and(|since| since.elapsed() > LOADING_MOST);
        if right_size || too_long {
            hide_loading();
        }
    }

    /// Show (true) or hide Krate's close capsule. The player shows it over
    /// every app it runs and hides it on Krate's own screens.
    ///
    /// A phone has no window close button, so an app had no way back. A
    /// gesture would collide with apps: a pinch is a map's zoom, an edge
    /// swipe a game's thumb stick, and neither can be discovered. A capsule
    /// drawn by Krate -- not by the app, so no app can hide or imitate it --
    /// is always there and means the same thing everywhere. Tapping it asks
    /// the app to close, as a desktop close button does; a second tap closes
    /// an app that does not answer.
    pub fn set_app_chrome(visible: bool) {
        APP_CHROME.store(visible, std::sync::atomic::Ordering::Relaxed);
        DispatchQueue::main().exec_async(move || {
            if let Some(main_side) = MAIN_SIDE.0.get() {
                main_side.close.setHidden(!visible);
            }
        });
    }

    static APP_CHROME: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// Which way up the app's window wants the screen: landscape when the
    /// app asked for a window wider than tall (a game), portrait otherwise.
    static LANDSCAPE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    define_class!(
        // SAFETY:
        // - UIViewController has no extra subclassing requirements for
        //   overriding its orientation query.
        // - UIKit calls it on the main thread.
        #[unsafe(super = UIViewController)]
        #[thread_kind = MainThreadOnly]
        #[ivars = ()]
        struct KrateViewController;

        // SAFETY: NSObjectProtocol has no additional requirements.
        unsafe impl NSObjectProtocol for KrateViewController {}

        impl KrateViewController {
            // SAFETY: signature matches UIViewController's.
            #[unsafe(method(supportedInterfaceOrientations))]
            fn supported_interface_orientations(&self) -> objc2_ui_kit::UIInterfaceOrientationMask {
                if LANDSCAPE.load(std::sync::atomic::Ordering::Relaxed) {
                    objc2_ui_kit::UIInterfaceOrientationMask::Landscape
                } else {
                    objc2_ui_kit::UIInterfaceOrientationMask::Portrait
                }
            }

            #[unsafe(method(prefersStatusBarHidden))]
            fn prefers_status_bar_hidden(&self) -> bool {
                LANDSCAPE.load(std::sync::atomic::Ordering::Relaxed)
            }

            #[unsafe(method(prefersHomeIndicatorAutoHidden))]
            fn prefers_home_indicator_auto_hidden(&self) -> bool {
                LANDSCAPE.load(std::sync::atomic::Ordering::Relaxed)
            }
        }
    );

    /// Turn the screen to suit a window of `size`: sideways for a window
    /// wider than tall. iOS animates the turn; this waits for it (up to
    /// 1.5 s), so the window an app gets already has the shape it asked
    /// for -- Counter-Strike read 390x844 for its first frame otherwise and
    /// sized its picture for a portrait screen.
    fn request_orientation(size: WindowSize) {
        let landscape = size.width > size.height;
        if LANDSCAPE.swap(landscape, std::sync::atomic::Ordering::Relaxed) == landscape {
            return;
        }
        let turned = |shared: &Shared| {
            shared
                .screen
                .is_some_and(|(l, _)| (l.width > l.height) == landscape)
        };
        DispatchQueue::main().exec_async(move || {
            let Some(main_side) = MAIN_SIDE.0.get() else {
                return;
            };
            let mask = if landscape {
                objc2_ui_kit::UIInterfaceOrientationMask::Landscape
            } else {
                objc2_ui_kit::UIInterfaceOrientationMask::Portrait
            };
            main_side.controller.setNeedsUpdateOfSupportedInterfaceOrientations();
            main_side.controller.setNeedsStatusBarAppearanceUpdate();
            if let Some(scene) = main_side.window.windowScene() {
                let prefs =
                    objc2_ui_kit::UIWindowSceneGeometryPreferencesIOS::initWithInterfaceOrientations(
                        <objc2_ui_kit::UIWindowSceneGeometryPreferencesIOS as objc2::AnyThread>::alloc(),
                        mask,
                    );
                scene.requestGeometryUpdateWithPreferences_errorHandler(&prefs, None);
            }
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
        let mut shared = SHARED.lock().expect("shared state lock");
        while !turned(&shared) {
            let now = std::time::Instant::now();
            if now >= deadline {
                break;
            }
            shared = INPUT_ARRIVED
                .wait_timeout(shared, deadline - now)
                .expect("shared state lock")
                .0;
        }
    }

    /// The view's bounds changed (the screen turned): move the GPU layer
    /// with it and publish the new size for the guest.
    fn view_laid_out(view: &KrateSurfaceView) {
        let uiview: &UIView = view;
        let bounds: CGRect = uiview.bounds();
        if let Some(main_side) = MAIN_SIDE.0.get() {
            objc2_quartz_core::CATransaction::begin();
            objc2_quartz_core::CATransaction::setDisableActions(true);
            main_side.metal.setFrame(bounds);
            objc2_quartz_core::CATransaction::commit();
            // The capsule: top right, inside the safe area (clear of the
            // notch and the status bar), above the app.
            let safe = uiview.safeAreaInsets();
            let (cw, ch) = (44.0, 30.0);
            let close: &UIView = &main_side.close;
            close.setFrame(CGRect::new(
                CGPoint::new(
                    bounds.size.width - safe.right - 12.0 - cw,
                    safe.top.max(8.0) + 6.0,
                ),
                objc2_core_foundation::CGSize::new(cw, ch),
            ));
            uiview.bringSubviewToFront(close);
            // The cover fills the view, its contents centred.
            let l = &main_side.loading;
            let (w, h) = (bounds.size.width, bounds.size.height);
            let frame = |x: f64, y: f64, fw: f64, fh: f64| {
                CGRect::new(
                    CGPoint::new(x, y),
                    objc2_core_foundation::CGSize::new(fw, fh),
                )
            };
            l.cover.setFrame(bounds);
            let mark: &UIView = &l.mark;
            mark.setFrame(frame(w / 2.0 - 36.0, h / 2.0 - 96.0, 72.0, 72.0));
            let title: &UIView = &l.title;
            title.setFrame(frame(16.0, h / 2.0 - 8.0, w - 32.0, 28.0));
            let status: &UIView = &l.status;
            status.setFrame(frame(16.0, h / 2.0 + 22.0, w - 32.0, 22.0));
            let spinner: &UIView = &l.spinner;
            spinner.setFrame(frame(w / 2.0 - 15.0, h / 2.0 + 58.0, 30.0, 30.0));
            if !l.cover.isHidden() {
                uiview.bringSubviewToFront(&l.cover);
            }
        }
        let Ok(logical) = WindowSize::new(
            (bounds.size.width as u32).max(1),
            (bounds.size.height as u32).max(1),
        ) else {
            return;
        };
        let mut shared = SHARED.lock().expect("shared state lock");
        if let Some((current, scale)) = shared.screen {
            if current != logical {
                shared.screen = Some((logical, scale));
                drop(shared);
                INPUT_ARRIVED.notify_all();
            }
        }
    }

    fn record_touches(view: &KrateSurfaceView, touches: &objc2_foundation::NSSet<UITouch>) {
        let uiview: &UIView = view;
        let mut shared = SHARED.lock().expect("shared state lock");
        for touch in touches.allObjects() {
            let location: CGPoint = unsafe { touch.locationInView(Some(uiview)) };
            let finger = Retained::as_ptr(&touch) as usize;
            shared.touches.push(RawTouch {
                phase: unsafe { touch.phase() },
                x: location.x as f32,
                y: location.y as f32,
                finger,
            });
        }
        drop(shared);
        diag("touch");
        INPUT_ARRIVED.notify_all();
        // Wake the guest again when a resting finger's press falls due.
        std::thread::spawn(|| {
            std::thread::sleep(PRESS_AFTER + std::time::Duration::from_millis(2));
            INPUT_ARRIVED.notify_all();
        });
    }

    /// Build the UIKit side, on the main queue, once.
    fn ensure_main_side() -> Result<(WindowSize, f32), UiAdapterError> {
        if let Some(screen) = SHARED.lock().expect("shared state lock").screen {
            return Ok(screen);
        }
        let mut result: Result<(WindowSize, f32), UiAdapterError> = Err(UiAdapterError::Internal(
            "main-queue closure never ran".to_string(),
        ));
        DispatchQueue::main().exec_sync(|| {
            result = (|| {
                let mtm = MainThreadMarker::new().ok_or_else(|| {
                    UiAdapterError::Internal(
                        "main-queue closure ran off the main thread".to_string(),
                    )
                })?;
                let screen = UIScreen::mainScreen(mtm);
                let bounds: CGRect = screen.bounds();
                // Capped at 2x deliberately: CPU-rasterizing full 3x spent
                // the whole frame budget; the view's GPU compositor does
                // the final stretch free (K-089).
                let scale = (screen.scale() as f32).min(2.0);

                let window = unsafe { UIWindow::initWithFrame(UIWindow::alloc(mtm), bounds) };
                // The window MUST join the connected UIWindowScene: since
                // iOS 13 an unattached window still renders but rides a
                // legacy event path that starves continuous touch delivery
                // -- a real 120 Hz phone produced 116 touch callouts in an
                // entire scrolling session before this line existed.
                unsafe {
                    let scenes = UIApplication::sharedApplication(mtm).connectedScenes();
                    for scene in scenes.allObjects() {
                        if let Ok(window_scene) = scene.downcast::<UIWindowScene>() {
                            window.setWindowScene(Some(&window_scene));
                            break;
                        }
                    }
                }
                let controller: Retained<KrateViewController> = {
                    let this = KrateViewController::alloc(mtm).set_ivars(());
                    unsafe { msg_send![super(this), init] }
                };
                let view: Retained<KrateSurfaceView> = {
                    let this = KrateSurfaceView::alloc(mtm).set_ivars(());
                    // SAFETY: UIView's initWithFrame is the designated
                    // initializer.
                    unsafe { msg_send![super(this), initWithFrame: bounds] }
                };
                unsafe {
                    view.setUserInteractionEnabled(true);
                    // Every finger, not only the first: a game moves with one
                    // thumb and aims with the other (krate:ui/touch). UIKit
                    // delivers one touch per view unless told otherwise.
                    view.setMultipleTouchEnabled(true);
                    let as_uiview: &UIView = &view;
                    controller.setView(Some(as_uiview));
                    let as_controller: &UIViewController = &controller;
                    window.setRootViewController(Some(as_controller));
                    window.makeKeyAndVisible();
                }
                let logical = WindowSize::new(
                    (bounds.size.width as u32).max(1),
                    (bounds.size.height as u32).max(1),
                )
                .map_err(|err| UiAdapterError::Unsupported(err.to_string()))?;

                // The GPU canvas draws into a CAMetalLayer sublayer sized
                // to the full view at FULL native density -- the 2x cap is
                // a CPU-raster economy the GPU does not need.
                // Cap the GPU raster at 2x too. Full 3x is 3.0 million
                // pixels per frame; vello's compute passes could not finish
                // one refresh in time, so get_current_texture waited for the
                // NEXT vsync -- measured at 26 ms of acquire and a hard 30
                // fps. At 2x the pixel work drops 2.25x and the layer's own
                // contentsScale still presents it sharp on a 3x panel.
                let native_scale = (screen.scale() as f32).min(2.0);
                let metal: objc2::rc::Retained<objc2_quartz_core::CAMetalLayer> =
                    unsafe { objc2_quartz_core::CAMetalLayer::new() };
                unsafe {
                    metal.setFrame(bounds);
                    metal.setContentsScale(native_scale as f64);
                    let as_uiview: &UIView = &view;
                    as_uiview.layer().addSublayer(&metal);
                }
                // The display link: the panel tells us when a frame is due.
                // Scheduled on the main run loop in the common modes so it
                // keeps firing during tracking (a finger on the screen is
                // exactly when frames matter most).
                let link_target = {
                    let this = KrateDisplayLinkTarget::alloc(mtm).set_ivars(());
                    let this: objc2::rc::Retained<KrateDisplayLinkTarget> =
                        unsafe { objc2::msg_send![super(this), init] };
                    this
                };
                let display_link = unsafe {
                    objc2_quartz_core::CADisplayLink::displayLinkWithTarget_selector(
                        &link_target,
                        objc2::sel!(krateDisplayLinkFired:),
                    )
                };
                // The panel's full rate: 120 Hz on a ProMotion iPhone (the
                // Info.plist opts in), so a list follows the finger the way
                // the phone's own apps do. iOS still lowers it for battery
                // when nothing moves.
                display_link.setPreferredFrameRateRange(objc2_quartz_core::CAFrameRateRange {
                    minimum: 60.0,
                    maximum: 120.0,
                    preferred: 120.0,
                });
                unsafe {
                    display_link.addToRunLoop_forMode(
                        &objc2_foundation::NSRunLoop::mainRunLoop(),
                        objc2_foundation::NSRunLoopCommonModes,
                    );
                }

                let metal_ptr = objc2::rc::Retained::as_ptr(&metal) as usize;
                SHARED.lock().expect("shared state lock").panel_scale = screen.scale() as f32;
                SHARED.lock().expect("shared state lock").metal_layer =
                    Some((metal_ptr, native_scale));

                // Krate's close capsule: dark glass, the system's own xmark,
                // above the app's drawing, hidden until the player runs an app.
                let close_target: Retained<KrateCloseTarget> = {
                    let this = KrateCloseTarget::alloc(mtm).set_ivars(());
                    unsafe { msg_send![super(this), init] }
                };
                let close =
                    objc2_ui_kit::UIButton::buttonWithType(objc2_ui_kit::UIButtonType::System, mtm);
                unsafe {
                    close.setImage_forState(
                        UIImage::systemImageNamed(&objc2_foundation::NSString::from_str("xmark"))
                            .as_deref(),
                        objc2_ui_kit::UIControlState::Normal,
                    );
                    close.setTintColor(Some(&objc2_ui_kit::UIColor::whiteColor()));
                    let as_view: &UIView = &close;
                    as_view.setBackgroundColor(Some(&objc2_ui_kit::UIColor::colorWithWhite_alpha(
                        0.0, 0.38,
                    )));
                    as_view.layer().setCornerRadius(15.0);
                    as_view.setHidden(!APP_CHROME.load(std::sync::atomic::Ordering::Relaxed));
                    close.addTarget_action_forControlEvents(
                        Some(&close_target),
                        objc2::sel!(krateClose:),
                        objc2_ui_kit::UIControlEvents::TouchUpInside,
                    );
                    let host: &UIView = &view;
                    host.addSubview(as_view);
                }
                // The loading cover: light, the mark, a title, a status line
                // and the spinner; hidden until something is loading.
                let loading = unsafe {
                    let cover = UIView::new(mtm);
                    cover.setBackgroundColor(Some(
                        &objc2_ui_kit::UIColor::colorWithRed_green_blue_alpha(
                            0.949, 0.953, 0.969, 1.0,
                        ),
                    ));
                    cover.setHidden(true);
                    let mark_image =
                        UIImage::imageNamed(&objc2_foundation::NSString::from_str("KrateMark"));
                    let mark =
                        UIImageView::initWithImage(UIImageView::alloc(mtm), mark_image.as_deref());
                    let label = |size: f64, weight: f64, grey: f64| {
                        let label = objc2_ui_kit::UILabel::new(mtm);
                        label.setFont(Some(&objc2_ui_kit::UIFont::systemFontOfSize_weight(
                            size, weight,
                        )));
                        label.setTextColor(Some(&objc2_ui_kit::UIColor::colorWithWhite_alpha(
                            grey, 1.0,
                        )));
                        label.setTextAlignment(objc2_ui_kit::NSTextAlignment::Center);
                        label
                    };
                    let title = label(20.0, objc2_ui_kit::UIFontWeightSemibold, 0.05);
                    let status = label(15.0, objc2_ui_kit::UIFontWeightRegular, 0.45);
                    let spinner =
                        objc2_ui_kit::UIActivityIndicatorView::initWithActivityIndicatorStyle(
                            objc2_ui_kit::UIActivityIndicatorView::alloc(mtm),
                            objc2_ui_kit::UIActivityIndicatorViewStyle::Medium,
                        );
                    for part in [
                        &*mark as &UIView,
                        &*title as &UIView,
                        &*status as &UIView,
                        &*spinner as &UIView,
                    ] {
                        cover.addSubview(part);
                    }
                    let host: &UIView = &view;
                    host.addSubview(&cover);
                    Loading {
                        cover,
                        mark,
                        title,
                        status,
                        spinner,
                    }
                };
                let _ = MAIN_SIDE.0.set(MainSide {
                    loading,
                    close,
                    _close_target: close_target,
                    metal,
                    _link_target: link_target,
                    _display_link: display_link,
                    window,
                    controller,
                    view,
                });
                Ok((logical, scale))
            })();
        });
        let screen = result?;
        SHARED.lock().expect("shared state lock").screen = Some(screen);
        Ok(screen)
    }

    fn with_guest<T>(
        f: impl FnOnce(&mut GuestSide) -> Result<T, UiAdapterError>,
    ) -> Result<T, UiAdapterError> {
        GUEST.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                let (logical, scale) = ensure_main_side()?;
                *slot = Some(GuestSide {
                    krate: None,
                    logical,
                    scale,
                    placements: Vec::new(),
                    events: Vec::new(),
                    pointer_samples: Vec::new(),
                    key_samples: Vec::new(),
                    wheel_samples: Vec::new(),
                    pending_scroll: None,
                    gesture: None,
                    fingers: Default::default(),
                    hovered: None,
                    pressed_widget: None,
                    paint_buffer: Vec::new(),
                    dirty: false,
                });
            }
            f(slot.as_mut().expect("guest side initialized"))
        })
    }

    fn guest_initialized() -> bool {
        GUEST.with(|slot| slot.borrow().is_some())
    }

    /// The tap-or-scroll synthesis: within the slop a touch is a tap
    /// (pointer press + release), past it every move is a wheel delta --
    /// the contract the Android adapter proved on-device.
    fn digest_touches(guest: &mut GuestSide) {
        const SLOP: f32 = 8.0;
        let raw: Vec<RawTouch> = {
            let mut shared = SHARED.lock().expect("shared state lock");
            std::mem::take(&mut shared.touches)
        };
        let Some(krate) = guest.krate else {
            return;
        };
        for touch in raw {
            match touch.phase {
                UITouchPhase::Ended | UITouchPhase::Cancelled => {
                    guest.fingers.lift(touch.finger as u64)
                }
                _ => guest.fingers.touch(touch.finger as u64, touch.x, touch.y),
            }
            match touch.phase {
                UITouchPhase::Began => {
                    if guest.gesture.is_none() {
                        guest.gesture = Some(TouchGesture {
                            finger: touch.finger,
                            start: (touch.x, touch.y),
                            last: (touch.x, touch.y),
                            scrolling: false,
                            began: std::time::Instant::now(),
                            pressed: false,
                        });
                        guest.hovered = krate_adapter_common::painter::topmost_interactive_at(
                            &guest.placements,
                            touch.x,
                            touch.y,
                        );
                    }
                }
                UITouchPhase::Moved => {
                    let same = guest
                        .gesture
                        .as_ref()
                        .is_some_and(|g| g.finger == touch.finger);
                    if same {
                        let gesture = guest.gesture.as_mut().expect("gesture checked");
                        if !gesture.scrolling {
                            let sx = touch.x - gesture.start.0;
                            let sy = touch.y - gesture.start.1;
                            if sx * sx + sy * sy > SLOP * SLOP {
                                gesture.scrolling = true;
                                // A press already sent is let go far outside
                                // the window, so nothing that acts on a
                                // release under the finger fires: the touch
                                // became a scroll.
                                if gesture.pressed {
                                    guest.pointer_samples.push(RawPointerSample {
                                        window: krate,
                                        x: -10_000.0,
                                        y: -10_000.0,
                                        pressed: false,
                                    });
                                }
                            }
                        }
                        let dx = touch.x - gesture.last.0;
                        let dy = touch.y - gesture.last.1;
                        gesture.last = (touch.x, touch.y);
                        if gesture.scrolling && (dx.abs() > f32::EPSILON || dy.abs() > f32::EPSILON)
                        {
                            let pending = guest
                                .pending_scroll
                                .get_or_insert((0.0, 0.0, touch.x, touch.y));
                            pending.0 -= dx;
                            pending.1 -= dy;
                            pending.2 = touch.x;
                            pending.3 = touch.y;
                        }
                    }
                }
                UITouchPhase::Ended => {
                    let same = guest
                        .gesture
                        .as_ref()
                        .is_some_and(|g| g.finger == touch.finger);
                    if same {
                        let gesture = guest.gesture.take().expect("gesture checked");
                        if !gesture.scrolling {
                            // A quick tap lifts before its press went out:
                            // both now, at the lift.
                            if !gesture.pressed {
                                guest.pointer_samples.push(RawPointerSample {
                                    window: krate,
                                    x: touch.x,
                                    y: touch.y,
                                    pressed: true,
                                });
                            }
                            guest.pointer_samples.push(RawPointerSample {
                                window: krate,
                                x: touch.x,
                                y: touch.y,
                                pressed: false,
                            });
                        }
                    }
                }
                UITouchPhase::Cancelled => {
                    let same = guest
                        .gesture
                        .as_ref()
                        .is_some_and(|g| g.finger == touch.finger);
                    if same {
                        guest.gesture = None;
                    }
                }
                _ => {}
            }
        }
    }

    /// A finger that has rested past PRESS_AFTER without scrolling presses
    /// now, where it landed.
    fn deliver_due_press(guest: &mut GuestSide) {
        let Some(krate) = guest.krate else {
            return;
        };
        if let Some(gesture) = guest.gesture.as_mut() {
            if !gesture.pressed && !gesture.scrolling && gesture.began.elapsed() >= PRESS_AFTER {
                gesture.pressed = true;
                guest.pointer_samples.push(RawPointerSample {
                    window: krate,
                    x: gesture.start.0,
                    y: gesture.start.1,
                    pressed: true,
                });
            }
        }
    }

    /// Rasterize on the guest thread, then hand finished pixels to the
    /// main queue, which only wraps them in a CGImage and sets the view.
    /// The autoreleasepool per frame is the macOS adapter's 46 GB lesson.
    fn blit(guest: &mut GuestSide) {
        diag("cpu-blit-start");
        let phys_w = (guest.logical.width as f32 * guest.scale) as usize;
        let phys_h = (guest.logical.height as f32 * guest.scale) as usize;
        if phys_w == 0 || phys_h == 0 {
            return;
        }
        guest.paint_buffer.clear();
        guest.paint_buffer.resize(phys_w * phys_h, 0);
        krate_adapter_common::painter::paint_placements(
            &mut guest.paint_buffer,
            phys_w as u32,
            phys_h as u32,
            guest.scale,
            &guest.placements,
            krate_adapter_common::painter::PaintInteraction {
                hovered: guest.hovered,
                pressed: guest.pressed_widget,
            },
        );

        let buffer = &mut guest.paint_buffer;
        let scale = guest.scale;
        DispatchQueue::main().exec_sync(|| {
            objc2::rc::autoreleasepool(|_| unsafe {
                let Some(main_side) = MAIN_SIDE.0.get() else {
                    return;
                };
                let color_space = CGColorSpaceCreateDeviceRGB();
                // 0xAARRGGBB u32s are BGRA in little-endian memory:
                // premultiplied-first + 32-little describes them exactly,
                // and alpha is always 0xFF.
                let bitmap_info =
                    CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
                let context = CGBitmapContextCreate(
                    buffer.as_mut_ptr().cast(),
                    phys_w,
                    phys_h,
                    8,
                    phys_w * 4,
                    color_space.as_deref(),
                    bitmap_info,
                );
                let Some(context) = context else {
                    return;
                };
                let Some(image) = CGBitmapContextCreateImage(Some(&context)) else {
                    return;
                };
                let ui_image = UIImage::imageWithCGImage_scale_orientation(
                    &image,
                    scale as f64,
                    objc2_ui_kit::UIImageOrientation::Up,
                );
                main_side.view.setImage(Some(&ui_image));
            });
        });
        guest.dirty = false;
        // A CPU frame lifts the loading cover only while it is what the
        // screen shows. Once the GPU draws the canvases, the CPU blit is
        // the empty window a new app has before its first canvas frame --
        // lifting the cover on it showed the last app's frame, stretched,
        // through the turn of the screen and the app's start.
        if !GPU_ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
            frame_shown(Some(guest.logical));
        }
    }

    // ------------------------------------------------------------------
    // The adapter surface. Every function runs on the guest thread.

    pub fn create_native_window(
        krate: WindowId,
        _title: &str,
        size: WindowSize,
    ) -> Result<(u64, WinitWindowSnapshot), UiAdapterError> {
        // The window exists first, then the screen turns to suit it, then
        // the guest learns the size it got.
        ensure_main_side()?;
        request_orientation(size);
        with_guest(|guest| {
            if let Some((logical, _)) = SHARED.lock().expect("shared state lock").screen {
                guest.logical = logical;
            }
            guest.krate = Some(krate);
            guest.placements.clear();
            guest.gesture = None;
            guest.pointer_samples.clear();
            guest.wheel_samples.clear();
            // The screen is the law: the next pump tells the guest its
            // real size, exactly as a resize would. The initial snapshot
            // repeats the requested size so the truth registers as a
            // change (the session diffs snapshots).
            guest
                .events
                .push((krate, WinitWindowNativeEvent::Resized(guest.logical)));
            let snapshot = WinitWindowSnapshot::new(krate, size, true, true, guest.scale)?;
            // 0 reads as a null handle to the shared validation; any fixed
            // nonzero token works -- never dereferenced.
            Ok((1, snapshot))
        })
    }

    pub fn set_drawn_placements(
        krate: WindowId,
        placements: &[WidgetPlacement],
    ) -> Result<usize, UiAdapterError> {
        if !guest_initialized() {
            return Ok(0);
        }
        with_guest(|guest| {
            if guest.krate != Some(krate) {
                return Ok(0);
            }
            guest.placements = placements
                .iter()
                .filter(|placement| krate_adapter_common::painter::drawn_kind(placement.kind))
                .cloned()
                .collect();
            let drawn = guest.placements.len();
            guest.dirty = true;
            if !GPU_ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
                blit(guest);
            }
            Ok(drawn)
        })
    }

    pub fn pump_native_events() -> Result<CollectedNativeEvents, UiAdapterError> {
        if !guest_initialized() {
            return Ok(Vec::new());
        }
        with_guest(|guest| {
            digest_touches(guest);
            let close = std::mem::take(&mut SHARED.lock().expect("shared state lock").close_tapped);
            if let (true, Some(krate)) = (close, guest.krate) {
                guest
                    .events
                    .push((krate, WinitWindowNativeEvent::CloseRequested));
            }
            let screen = SHARED.lock().expect("shared state lock").screen;
            if let (Some((logical, _)), Some(krate)) = (screen, guest.krate) {
                if logical != guest.logical {
                    guest.logical = logical;
                    guest.dirty = true;
                    guest
                        .events
                        .push((krate, WinitWindowNativeEvent::Resized(logical)));
                }
            }
            Ok(std::mem::take(&mut guest.events))
        })
    }

    /// Park the guest thread until input arrives or the deadline passes.
    /// The condvar is signaled by the main thread's touch callouts, so a
    /// finger wakes the guest immediately -- no run loop involved.
    /// Park until the panel's next refresh, or `max`. Touches do NOT wake
    /// this: pacing that wakes on input lets a fast finger present faster
    /// than the display consumes and stalls the next acquire (K-090).
    pub fn park_for_frame(max: std::time::Duration) -> bool {
        if !guest_initialized() {
            return false;
        }
        if VSYNC_PENDING.swap(false, std::sync::atomic::Ordering::Relaxed) {
            return true;
        }
        let deadline = std::time::Instant::now() + max;
        loop {
            let now = std::time::Instant::now();
            if now >= deadline {
                return true;
            }
            let shared = SHARED.lock().expect("shared state lock");
            let (_guard, _timeout) = INPUT_ARRIVED
                .wait_timeout(shared, deadline - now)
                .expect("shared state lock");
            if VSYNC_PENDING.swap(false, std::sync::atomic::Ordering::Relaxed) {
                return true;
            }
            // A touch woke us, not the panel. Keep waiting for the frame.
        }
    }

    pub fn park_for_events(max: std::time::Duration) -> bool {
        if !guest_initialized() {
            return false;
        }
        // A frame is already due, or input is waiting: do not park at all.
        if VSYNC_PENDING.swap(false, std::sync::atomic::Ordering::Relaxed) {
            return true;
        }
        let shared = SHARED.lock().expect("shared state lock");
        if !shared.touches.is_empty() {
            return true;
        }
        // Wake on whichever comes first, a finger or the panel's refresh.
        let (_guard, _timeout) = INPUT_ARRIVED
            .wait_timeout(shared, max)
            .expect("shared state lock");
        VSYNC_PENDING.store(false, std::sync::atomic::Ordering::Relaxed);
        true
    }

    /// Every finger on the window now, in logical pixels.
    pub fn touches(_id: WindowId) -> Vec<(u64, f32, f32)> {
        if !guest_initialized() {
            return Vec::new();
        }
        with_guest(|guest| {
            digest_touches(guest);
            Ok(guest.fingers.read())
        })
        .unwrap_or_default()
    }

    pub fn drain_pointer_samples() -> Vec<RawPointerSample> {
        if !guest_initialized() {
            return Vec::new();
        }
        with_guest(|guest| {
            digest_touches(guest);
            deliver_due_press(guest);
            Ok(std::mem::take(&mut guest.pointer_samples))
        })
        .unwrap_or_default()
    }

    pub fn drain_key_samples() -> Vec<RawKeySample> {
        if !guest_initialized() {
            return Vec::new();
        }
        with_guest(|guest| {
            let typed = std::mem::take(&mut SHARED.lock().expect("shared state lock").typed);
            if let Some(window) = guest.krate {
                for t in typed {
                    let (key, text) = match t {
                        Typed::Text(text) => {
                            // One press per character, each carrying its text,
                            // the way a physical keyboard delivers them.
                            for c in text.chars() {
                                let key = c.to_string();
                                for pressed in [true, false] {
                                    guest.key_samples.push(RawKeySample {
                                        window,
                                        key: key.clone(),
                                        pressed,
                                        modifiers: Default::default(),
                                        text: pressed.then(|| key.clone()),
                                    });
                                }
                            }
                            continue;
                        }
                        Typed::Backspace => ("Backspace", None),
                        Typed::Enter => ("Enter", None::<String>),
                    };
                    for pressed in [true, false] {
                        guest.key_samples.push(RawKeySample {
                            window,
                            key: key.to_string(),
                            pressed,
                            modifiers: Default::default(),
                            text: text.clone(),
                        });
                    }
                }
            }
            Ok(std::mem::take(&mut guest.key_samples))
        })
        .unwrap_or_default()
    }

    /// Show or put away the on-screen keyboard. True when it is showing.
    pub fn set_text_entry(_id: WindowId, on: bool) -> bool {
        if !guest_initialized() {
            return false;
        }
        let mut showing = false;
        DispatchQueue::main().exec_sync(|| {
            if let Some(main_side) = MAIN_SIDE.0.get() {
                let view: &UIView = &main_side.view;
                showing = if on {
                    view.becomeFirstResponder()
                } else {
                    view.resignFirstResponder();
                    false
                };
            }
        });
        showing
    }

    pub fn drain_wheel_samples() -> Vec<RawWheelSample> {
        if !guest_initialized() {
            return Vec::new();
        }
        with_guest(|guest| {
            let mut samples = std::mem::take(&mut guest.wheel_samples);
            if let (Some((dx, dy, x, y)), Some(krate)) = (guest.pending_scroll.take(), guest.krate)
            {
                diag(&format!("wheel dy={dy:.0}"));
                samples.push(RawWheelSample {
                    window: krate,
                    x,
                    y,
                    dx,
                    dy,
                    modifiers: Default::default(),
                });
            }
            Ok(samples)
        })
        .unwrap_or_default()
    }

    pub fn show_native_window(krate: WindowId) -> Result<bool, UiAdapterError> {
        if !guest_initialized() {
            return Ok(false);
        }
        with_guest(|guest| Ok(guest.krate == Some(krate)))
    }

    pub fn set_native_window_title(krate: WindowId, _title: &str) -> Result<bool, UiAdapterError> {
        // iOS windows have no title bars; accepting the call is the honest
        // no-op.
        if !guest_initialized() {
            return Ok(false);
        }
        with_guest(|guest| Ok(guest.krate == Some(krate)))
    }

    pub fn close_native_window(krate: WindowId) -> Result<bool, UiAdapterError> {
        if !guest_initialized() {
            return Ok(false);
        }
        with_guest(|guest| {
            if guest.krate == Some(krate) {
                guest.krate = None;
                guest.placements.clear();
                guest.dirty = true;
                blit(guest);
                Ok(true)
            } else {
                Ok(false)
            }
        })
    }

    pub fn has_native_window(krate: WindowId) -> Result<bool, UiAdapterError> {
        if !guest_initialized() {
            return Ok(false);
        }
        with_guest(|guest| Ok(guest.krate == Some(krate)))
    }

    pub fn redraw_all() -> Result<(), UiAdapterError> {
        if !guest_initialized() {
            return Ok(());
        }
        with_guest(|guest| {
            if guest.dirty {
                blit(guest);
            }
            Ok(())
        })
    }

    thread_local! {
        static GPU: RefCell<Option<crate::vello_canvas::GpuCanvas>> =
            const { RefCell::new(None) };
    }

    /// True once the GPU renders this app's frames: the CPU blit then has
    /// nothing to show and painting 1.4 million invisible pixels per frame
    /// is pure heat.
    static GPU_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// Render one recorded canvas frame on the GPU. True when claimed;
    /// false hands the frame back to the CPU path (init failed, wrong
    /// window, not a list this consumer knows).
    pub fn present_canvas_list(
        krate: WindowId,
        _widget: krate_adapter_common::ui::WidgetId,
        list: &krate_adapter_common::ui::CanvasListHandle,
    ) -> bool {
        if !guest_initialized() {
            return false;
        }
        let Some(list) = list.downcast_ref::<krate_adapter_common::canvas_list::CanvasList>()
        else {
            return false;
        };
        let owns = with_guest(|guest| Ok(guest.krate == Some(krate))).unwrap_or(false);
        if !owns {
            return false;
        }
        GPU.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                let Some(((layer, native_scale), logical)) = ({
                    let shared = SHARED.lock().expect("shared state lock");
                    shared.metal_layer.zip(shared.screen.map(|(l, _)| l))
                }) else {
                    return false;
                };
                let width = (logical.width as f32 * native_scale) as u32;
                let height = (logical.height as f32 * native_scale) as u32;
                // wgpu panics on validation gaps (the simulator's Metal
                // lacks indirect execution, for one); a missing GPU must
                // mean CPU fallback, never a dead app.
                let created = std::panic::catch_unwind(|| {
                    crate::vello_canvas::GpuCanvas::new(
                        layer as *mut std::ffi::c_void,
                        width,
                        height,
                        native_scale,
                    )
                })
                .unwrap_or_else(|_| Err("gpu init panicked (unsupported device)".into()));
                match created {
                    Ok(gpu) => {
                        GPU_ACTIVE.store(true, std::sync::atomic::Ordering::Relaxed);
                        // The image view would cover the metal layer with
                        // its last CPU frame; hide it once the GPU owns
                        // the pixels.
                        DispatchQueue::main().exec_sync(|| {
                            if let Some(main_side) = MAIN_SIDE.0.get() {
                                // The stale CPU frame must not fight the
                                // metal layer -- clearing the image is
                                // what stops the flashing.
                                main_side.view.setImage(None);
                            }
                        });
                        *slot = Some(gpu);
                    }
                    Err(why) => {
                        eprintln!("krate-ios: gpu canvas unavailable: {why}");
                        return false;
                    }
                }
            }
            diag("gpu-present-start");
            let gpu = slot.as_mut().expect("gpu initialized");
            // The drawable follows the view's size (the screen turned) and
            // the kind of frame: a whole-frame picture (a game, an emulator)
            // at the panel's full scale, so it is stretched once, by us,
            // crisply when the ratio is whole; interface frames at the 2x
            // cap the vector renderer can keep up with.
            let wanted = {
                let shared = SHARED.lock().expect("shared state lock");
                shared
                    .metal_layer
                    .zip(shared.screen.map(|(l, _)| l))
                    .map(|(layer, logical)| (layer, logical, shared.panel_scale))
            };
            if let Some(((_, native_scale), logical, panel_scale)) = wanted {
                let picture = crate::vello_canvas::whole_frame_picture(list).is_some();
                let scale = if picture { panel_scale } else { native_scale };
                if gpu.layer_scale() != scale {
                    DispatchQueue::main().exec_sync(|| {
                        if let Some(main_side) = MAIN_SIDE.0.get() {
                            main_side.metal.setContentsScale(scale as f64);
                        }
                    });
                    gpu.set_layer_scale(scale);
                }
                gpu.resize(
                    (logical.width as f32 * scale) as u32,
                    (logical.height as f32 * scale) as u32,
                );
            }
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| gpu.present(list)))
                    .unwrap_or_else(|_| Err("gpu present panicked".into()));
            match outcome {
                Ok(()) => {
                    diag("gpu-present-done");
                    frame_shown(WindowSize::new(list.logical_width, list.logical_height).ok());
                    true
                }
                Err(why) => {
                    eprintln!("krate-ios: gpu present failed: {why}");
                    false
                }
            }
        })
    }

    pub fn window_scale(krate: WindowId) -> f32 {
        if !guest_initialized() {
            return 1.0;
        }
        with_guest(|guest| {
            Ok(if guest.krate == Some(krate) {
                guest.scale
            } else {
                1.0
            })
        })
        .unwrap_or(1.0)
    }
}

#[cfg(not(target_os = "ios"))]
mod stub {
    use super::*;

    fn unsupported<T>() -> Result<T, UiAdapterError> {
        Err(UiAdapterError::Unsupported(
            "the UIKit backend only exists in iOS builds".to_string(),
        ))
    }

    pub fn create_native_window(
        _krate: WindowId,
        _title: &str,
        _size: WindowSize,
    ) -> Result<(u64, WinitWindowSnapshot), UiAdapterError> {
        unsupported()
    }

    pub fn set_drawn_placements(
        _krate: WindowId,
        _placements: &[WidgetPlacement],
    ) -> Result<usize, UiAdapterError> {
        Ok(0)
    }

    pub fn pump_native_events() -> Result<CollectedNativeEvents, UiAdapterError> {
        Ok(Vec::new())
    }

    pub fn park_for_frame(_max: std::time::Duration) -> bool {
        false
    }

    pub fn park_for_events(_max: std::time::Duration) -> bool {
        false
    }

    pub fn touches(_id: WindowId) -> Vec<(u64, f32, f32)> {
        Vec::new()
    }

    pub fn drain_pointer_samples() -> Vec<RawPointerSample> {
        Vec::new()
    }

    pub fn drain_key_samples() -> Vec<RawKeySample> {
        Vec::new()
    }

    pub fn set_text_entry(_id: WindowId, _on: bool) -> bool {
        false
    }

    pub fn set_app_chrome(_visible: bool) {}

    pub fn show_loading(_title: &str, _status: &str) {}

    pub fn hide_loading() {}

    pub fn drain_wheel_samples() -> Vec<RawWheelSample> {
        Vec::new()
    }

    pub fn show_native_window(_krate: WindowId) -> Result<bool, UiAdapterError> {
        Ok(false)
    }

    pub fn set_native_window_title(_krate: WindowId, _title: &str) -> Result<bool, UiAdapterError> {
        Ok(false)
    }

    pub fn close_native_window(_krate: WindowId) -> Result<bool, UiAdapterError> {
        Ok(false)
    }

    pub fn has_native_window(_krate: WindowId) -> Result<bool, UiAdapterError> {
        Ok(false)
    }

    pub fn redraw_all() -> Result<(), UiAdapterError> {
        Ok(())
    }

    pub fn window_scale(_krate: WindowId) -> f32 {
        1.0
    }

    pub fn present_canvas_list(
        _krate: WindowId,
        _widget: krate_adapter_common::ui::WidgetId,
        _list: &krate_adapter_common::ui::CanvasListHandle,
    ) -> bool {
        false
    }
}
