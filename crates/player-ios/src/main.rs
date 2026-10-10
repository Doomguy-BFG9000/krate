//! Krate for iOS: the app a person opens Krate apps with.
//!
//! A `.krate` someone sends -- in Messages, Mail, AirDrop or Files -- opens
//! here ("Open in Krate"). Krate keeps it in the person's library, asks
//! with a native iOS alert what the app may use, and runs it. Tapping the
//! Krate icon shows the library: every app received, and any app this
//! build carries.
//!
//! Two engines, one rule from Apple (2.5.2): code that arrives after
//! install may only be interpreted, so a received app runs on Pulley,
//! wasmtime's interpreter. An app compiled on our machines and built into
//! this binary (see build.rs) is native code signed with Krate, and runs
//! at full speed -- including when the same app arrives as a `.krate`:
//! its code is matched by hash, and the carried native code runs instead.
//!
//! Startup is the one genuinely iOS-shaped part. UIApplicationMain owns
//! the process and never returns, so the player's real main runs on its
//! own thread from the app delegate's first `applicationDidBecomeActive:`.
//! When the person leaves, the process exits -- the app is the process,
//! same as Android -- so every open is a fresh launch.

#[cfg(target_os = "ios")]
// The app-delegate URL callback: this app runs without a scene manifest, so
// that is where "Open in Krate" arrives, though UIKit marks its key type
// deprecated in favour of the scene lifecycle.
#[allow(deprecated)]
mod player {
    use objc2::{define_class, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly};
    use objc2_foundation::{NSDictionary, NSObject, NSObjectProtocol, NSString, NSURL};
    use objc2_ui_kit::{
        UIAlertAction, UIAlertActionStyle, UIAlertController, UIAlertControllerStyle,
        UIApplication, UIApplicationDelegate, UIApplicationOpenURLOptionsKey,
        UIDocumentPickerDelegate, UIViewController, UIWindow, UIWindowScene,
    };
    use std::cell::Cell;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    /// The app this build carries as native code, if any: compiled on our
    /// machines and linked into this binary's executable text, so it is
    /// signed with Krate and runs at native speed where iOS lets nothing
    /// become executable at run time. The 16 KB alignment is the iPhone's
    /// page size.
    #[cfg(krate_native_image)]
    mod native {
        const N: usize = include_bytes!(env!("KRATE_NATIVE_IMAGE")).len();
        #[repr(C, align(16384))]
        pub struct Aligned(pub [u8; N]);
        #[link_section = "__TEXT,__krate_app,regular,pure_instructions"]
        pub static IMAGE: Aligned = Aligned(*include_bytes!(env!("KRATE_NATIVE_IMAGE")));
        pub const MANIFEST: &str = include_str!(env!("KRATE_NATIVE_MANIFEST"));
        /// The SHA-256 of the app's WebAssembly, so the same app arriving
        /// as a `.krate` is recognised and runs as this native code.
        pub const SHA256: &str = env!("KRATE_NATIVE_SHA256");
    }

    const WALL_WASM: &[u8] =
        include_bytes!("../../../apps/krate-wall/target/wasm32-wasip1/release/krate_wall.wasm");
    const WALL_MANIFEST: &str = include_str!("../../../apps/krate-wall/manifest.toml");
    const HOME_WASM: &[u8] =
        include_bytes!("../../../apps/krate-home/target/wasm32-wasip1/release/krate_home.wasm");
    const HOME_MANIFEST: &str = include_str!("../../../apps/krate-home/manifest.toml");

    /// Larger than any Krate app a phone should be handed.
    const MAX_BUNDLE_BYTES: u64 = 64 * 1024 * 1024;

    /// A file someone opened with Krate, waiting for the guest thread.
    static OPENED: Mutex<Option<PathBuf>> = Mutex::new(None);

    pub struct DelegateState {
        started: Cell<bool>,
    }

    define_class!(
        // SAFETY:
        // - NSObject has no subclassing requirements for an app delegate.
        // - UIKit calls the delegate on the main thread.
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        #[ivars = DelegateState]
        struct KrateAppDelegate;

        unsafe impl NSObjectProtocol for KrateAppDelegate {}

        impl KrateAppDelegate {
            #[unsafe(method_id(init))]
            fn init(this: objc2::rc::Allocated<Self>) -> objc2::rc::Retained<Self> {
                let this = this.set_ivars(DelegateState {
                    started: Cell::new(false),
                });
                unsafe { objc2::msg_send![super(this), init] }
            }
        }

        unsafe impl UIApplicationDelegate for KrateAppDelegate {
            /// "Open in Krate" from Messages, Mail, AirDrop or Files: iOS
            /// copies the file into Documents/Inbox and hands over its URL,
            /// before the app becomes active.
            #[unsafe(method(application:openURL:options:))]
            fn open_url(
                &self,
                _application: &UIApplication,
                url: &NSURL,
                _options: &NSDictionary<UIApplicationOpenURLOptionsKey, objc2::runtime::AnyObject>,
            ) -> objc2::runtime::Bool {
                let Some(path) = url.path() else {
                    return objc2::runtime::Bool::NO;
                };
                *OPENED.lock().expect("opened lock") = Some(PathBuf::from(path.to_string()));
                objc2::runtime::Bool::YES
            }

            #[unsafe(method(applicationDidEnterBackground:))]
            fn did_enter_background(&self, _application: &UIApplication) {
                std::process::exit(0);
            }

            #[unsafe(method(applicationDidBecomeActive:))]
            fn did_become_active(&self, _application: &UIApplication) {
                if self.ivars().started.replace(true) {
                    return;
                }
                std::thread::Builder::new()
                    .name("krate-guest".to_string())
                    .stack_size(8 * 1024 * 1024)
                    .spawn(|| {
                        unsafe {
                            libc::pthread_set_qos_class_self_np(
                                libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE,
                                0,
                            );
                        }
                        run_player();
                        std::process::exit(0);
                    })
                    .expect("spawn the guest thread");
            }
        }
    );

    pub fn main() {
        let mtm =
            objc2::MainThreadMarker::new().expect("the iOS player's main runs on the main thread");
        let delegate_class = NSString::from_class(KrateAppDelegate::class());
        UIApplication::main(None, Some(&delegate_class), mtm);
    }

    /// An app the person can open.
    enum App {
        /// The app this build carries as native code.
        #[cfg(krate_native_image)]
        Carried,
        /// A `.krate` in the library.
        Received(PathBuf),
    }

    fn run_player() {
        let data_root = std::env::var("HOME")
            .map(|home| PathBuf::from(home).join("Documents"))
            .unwrap_or_else(|_| PathBuf::from("."));
        let library = data_root.join("Library");
        let _ = std::fs::create_dir_all(&library);

        let opened = OPENED.lock().expect("opened lock").take();
        let mut next = opened.and_then(|path| receive(&path, &library, &data_root));
        loop {
            let app = match next.take() {
                Some(app) => app,
                None => match home(&library, &data_root) {
                    Some(app) => app,
                    None => return,
                },
            };
            launch(app, &data_root);
        }
    }

    /// Keep a received `.krate` in the library, named for its app, so the
    /// home screen lists it and a newer copy replaces an older one.
    fn receive(path: &Path, library: &Path, data_root: &Path) -> Option<App> {
        let kept = (|| -> Result<PathBuf, String> {
            let size = std::fs::metadata(path)
                .map_err(|_| "The file is not there any more.".to_string())?
                .len();
            if size > MAX_BUNDLE_BYTES {
                return Err("This file is larger than any Krate app.".to_string());
            }
            let open = krate_bundle::open(path).map_err(|err| match err {
                krate_bundle::BundleError::NeedsNewerKrate { .. } => format!("{err}"),
                _ => {
                    "This file is not a Krate app, or it was damaged on the way. Ask for it again."
                        .to_string()
                }
            })?;
            let manifest = std::fs::read_to_string(open.manifest_path()).unwrap_or_default();
            let manifest = krate_manifest::Manifest::parse(&manifest)
                .map_err(|_| "This app's description could not be read.".to_string())?;
            let into = library.join(format!("{}.krate", sanitize(&manifest.app.id)));
            std::fs::copy(path, &into).map_err(|err| format!("Could not keep the app: {err}"))?;
            Ok(into)
        })();
        // iOS put the file in Documents/Inbox for us; it is ours to remove.
        let _ = std::fs::remove_file(path);
        match kept {
            Ok(into) => Some(App::Received(into)),
            Err(why) => {
                notice(data_root, "Could not open this file", &why);
                None
            }
        }
    }

    /// Krate's own screens: Library and Discover (apps/krate-home). The
    /// player prepares what they show -- the library, the hub catalogue,
    /// decoded screenshots -- and acts on the answer: open an app, get one
    /// from the hub, or pick a `.krate` in Files. None when the person
    /// leaves.
    fn home(library: &Path, data_root: &Path) -> Option<App> {
        let ui = data_root.join("home-ui");
        let art_dir = ui.join("art");
        let _ = std::fs::create_dir_all(&art_dir);
        let mut start = None;
        loop {
            let hub_apps = crate::hub::catalogue(&ui);
            let hub_art: std::collections::HashMap<String, String> = hub_apps
                .iter()
                .filter_map(|app| Some((app.id.clone(), crate::hub::art(app, &art_dir)?)))
                .collect();

            let mut apps: Vec<App> = Vec::new();
            let mut rows: Vec<String> = Vec::new();
            #[cfg(krate_native_image)]
            if let Ok(manifest) = krate_manifest::Manifest::parse(native::MANIFEST) {
                apps.push(App::Carried);
                rows.push(format!(
                    "{}\tBuilt in \u{00b7} Runs at full speed\t-",
                    field(&manifest.app.name)
                ));
            }
            let mut received: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(library)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "krate"))
                .map(|path| {
                    let at = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::UNIX_EPOCH);
                    (at, path)
                })
                .collect();
            received.sort_by(|a, b| b.0.cmp(&a.0));
            for (_, path) in received {
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                let name = krate_bundle::open(&path)
                    .ok()
                    .and_then(|open| std::fs::read_to_string(open.manifest_path()).ok())
                    .and_then(|text| krate_manifest::Manifest::parse(&text).ok())
                    .map(|manifest| manifest.app.name);
                let Some(name) = name else {
                    continue;
                };
                // An app that came from the hub shows its hub picture: the
                // hub names apps by the SHA-256 of their bytes.
                let art = hub_art
                    .get(&krate_bundle::sha256_hex(&bytes))
                    .map(String::as_str)
                    .unwrap_or("-");
                rows.push(format!(
                    "{}\tReceived \u{00b7} {}\t{art}",
                    field(&name),
                    human_size(bytes.len() as u64)
                ));
                apps.push(App::Received(path));
            }
            let _ = std::fs::write(ui.join("library.txt"), rows.join("\n") + "\n");
            let discover: Vec<String> = hub_apps
                .iter()
                .map(|app| {
                    format!(
                        "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                        app.id,
                        field(&app.name),
                        field(&app.author),
                        field(if app.category.is_empty() {
                            "apps"
                        } else {
                            &app.category
                        }),
                        field(&app.description),
                        hub_art.get(&app.id).map(String::as_str).unwrap_or("-"),
                        human_size(app.size),
                    )
                })
                .collect();
            let _ = std::fs::write(ui.join("discover.txt"), discover.join("\n") + "\n");
            match start.take() {
                Some(tab) => {
                    let _ = std::fs::write(ui.join("start.txt"), tab);
                }
                None => {
                    let _ = std::fs::remove_file(ui.join("start.txt"));
                }
            }

            let answer = run_trusted(HOME_WASM, HOME_MANIFEST, &ui, Vec::new())?;
            let answer = answer.lines().find(|l| l.starts_with("home:"))?.to_string();
            if let Some(n) = answer.strip_prefix("home:open:") {
                let n: usize = n.trim().parse().ok()?;
                return (n < apps.len()).then(|| apps.swap_remove(n));
            }
            if let Some(id) = answer.strip_prefix("home:get:") {
                let id: String = id.trim().chars().filter(char::is_ascii_hexdigit).collect();
                match crate::hub::download(&id, MAX_BUNDLE_BYTES) {
                    Ok(bytes) => {
                        let downloads = data_root.join("Downloads");
                        let _ = std::fs::create_dir_all(&downloads);
                        let file = downloads.join(format!("{id}.krate"));
                        if std::fs::write(&file, bytes).is_ok() {
                            if let Some(app) = receive(&file, library, data_root) {
                                return Some(app);
                            }
                        }
                    }
                    Err(why) => notice(
                        data_root,
                        "Could not download this app",
                        &format!("{why}. Check the connection and try again."),
                    ),
                }
                start = Some("discover");
                continue;
            }
            if answer.trim() == "home:files" {
                if let Some(path) = pick_file() {
                    if let Some(app) = receive(&path, library, data_root) {
                        return Some(app);
                    }
                }
                continue;
            }
            return None;
        }
    }

    /// A value for one tab-separated field: no tabs or line breaks inside.
    fn field(text: &str) -> String {
        text.replace(['\t', '\n', '\r'], " ")
    }

    fn human_size(bytes: u64) -> String {
        if bytes >= 1024 * 1024 {
            format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
        } else {
            format!("{} KB", bytes.div_ceil(1024).max(1))
        }
    }

    /// The iOS Files picker, for a `.krate` saved anywhere Files can see.
    /// iOS copies the pick into the app (import mode); None when cancelled.
    fn pick_file() -> Option<PathBuf> {
        let (tx, rx) = std::sync::mpsc::channel::<Option<PathBuf>>();
        dispatch2::DispatchQueue::main().exec_async(move || present_picker(tx));
        rx.recv().ok().flatten()
    }

    pub struct PickerState {
        tx: std::cell::RefCell<Option<std::sync::mpsc::Sender<Option<PathBuf>>>>,
        window: std::cell::RefCell<Option<objc2::rc::Retained<UIWindow>>>,
    }

    define_class!(
        // SAFETY:
        // - NSObject has no subclassing requirements for a delegate.
        // - UIKit calls the picker's delegate on the main thread.
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        #[ivars = PickerState]
        struct KratePickerDelegate;

        unsafe impl NSObjectProtocol for KratePickerDelegate {}

        unsafe impl UIDocumentPickerDelegate for KratePickerDelegate {
            #[unsafe(method(documentPicker:didPickDocumentsAtURLs:))]
            fn picked(
                &self,
                _controller: &objc2_ui_kit::UIDocumentPickerViewController,
                urls: &objc2_foundation::NSArray<NSURL>,
            ) {
                let path = urls
                    .firstObject()
                    .and_then(|url| url.path())
                    .map(|p| PathBuf::from(p.to_string()));
                self.finish(path);
            }

            #[unsafe(method(documentPickerWasCancelled:))]
            fn cancelled(&self, _controller: &objc2_ui_kit::UIDocumentPickerViewController) {
                self.finish(None);
            }
        }
    );

    impl KratePickerDelegate {
        fn finish(&self, path: Option<PathBuf>) {
            if let Some(window) = self.ivars().window.borrow_mut().take() {
                window.setHidden(true);
            }
            if let Some(tx) = self.ivars().tx.borrow_mut().take() {
                let _ = tx.send(path);
            }
            PICKER.with(|slot| slot.borrow_mut().take());
        }
    }

    thread_local! {
        /// The live picker delegate: UIKit holds delegates weakly.
        static PICKER: std::cell::RefCell<Option<objc2::rc::Retained<KratePickerDelegate>>> =
            const { std::cell::RefCell::new(None) };
    }

    #[allow(deprecated)]
    fn present_picker(tx: std::sync::mpsc::Sender<Option<PathBuf>>) {
        let Some(mtm) = MainThreadMarker::new() else {
            let _ = tx.send(None);
            return;
        };
        let Some(window) = overlay_window(mtm) else {
            let _ = tx.send(None);
            return;
        };
        let types = objc2_foundation::NSArray::from_retained_slice(&[NSString::from_str(
            "dev.krate.bundle",
        )]);
        let picker = objc2_ui_kit::UIDocumentPickerViewController::initWithDocumentTypes_inMode(
            objc2_ui_kit::UIDocumentPickerViewController::alloc(mtm),
            &types,
            objc2_ui_kit::UIDocumentPickerMode::Import,
        );
        let delegate: objc2::rc::Retained<KratePickerDelegate> = {
            let this = KratePickerDelegate::alloc(mtm).set_ivars(PickerState {
                tx: std::cell::RefCell::new(Some(tx)),
                window: std::cell::RefCell::new(Some(window.clone())),
            });
            unsafe { objc2::msg_send![super(this), init] }
        };
        picker.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*delegate)));
        PICKER.with(|slot| *slot.borrow_mut() = Some(delegate));
        if let Some(root) = window.rootViewController() {
            root.presentViewController_animated_completion(&picker, true, None);
        }
    }

    /// A window above everything, for a system sheet (an alert, a picker).
    fn overlay_window(mtm: MainThreadMarker) -> Option<objc2::rc::Retained<UIWindow>> {
        let scene = UIApplication::sharedApplication(mtm)
            .connectedScenes()
            .allObjects()
            .into_iter()
            .find_map(|scene| scene.downcast::<UIWindowScene>().ok())?;
        let window = UIWindow::initWithWindowScene(UIWindow::alloc(mtm), &scene);
        let root = UIViewController::new(mtm);
        unsafe {
            window.setWindowLevel(objc2_ui_kit::UIWindowLevelAlert);
        }
        window.setRootViewController(Some(&root));
        window.makeKeyAndVisible();
        Some(window)
    }

    /// Ask, then run. Every open asks: the answer is never remembered.
    fn launch(app: App, data_root: &Path) {
        // What runs, and its manifest.
        let mut _opened_bundle = None;
        let (manifest_text, wasm, assets_root) = match &app {
            #[cfg(krate_native_image)]
            App::Carried => (native::MANIFEST.to_string(), None, None),
            App::Received(path) => {
                let open = match krate_bundle::open(path) {
                    Ok(open) => open,
                    Err(err) => {
                        notice(data_root, "This app did not open", &format!("{err}"));
                        return;
                    }
                };
                let manifest = std::fs::read_to_string(open.manifest_path()).unwrap_or_default();
                let wasm = std::fs::read(open.component_path()).ok();
                let assets = open.assets_path().map(Path::to_path_buf);
                _opened_bundle = Some(open);
                (manifest, wasm, assets)
            }
        };
        let manifest = match krate_manifest::Manifest::parse(&manifest_text) {
            Ok(manifest) => manifest,
            Err(_) => {
                notice(
                    data_root,
                    "This app did not open",
                    "Its description could not be read.",
                );
                return;
            }
        };

        let Some(all) = ask_permission(&manifest) else {
            return;
        };
        let declared = match manifest.declared_capabilities() {
            Ok(declared) => declared,
            Err(err) => {
                notice(data_root, "This app did not open", &format!("{err:#}"));
                return;
            }
        };
        let kept: Vec<_> = manifest
            .capabilities
            .iter()
            .zip(declared)
            .filter(|(request, _)| all || request.required)
            .map(|(_, capability)| capability)
            .collect();

        let sandbox = data_root.join("apps").join(sanitize(&manifest.app.id));
        let _ = std::fs::create_dir_all(&sandbox);
        let mut config = krate_runtime::Config {
            session_policy: krate_policy::SessionPolicy::from_grants(kept),
            sandbox_root: sandbox,
            bundle_assets_root: assets_root,
            phase3_ui_mode: krate_runtime::phase3_ui::Phase3HostUiMode::NativePrototype,
            ..Default::default()
        };
        if let Some(mib) = memory_budget_mib(&manifest) {
            config.memory_bytes = mib * 1024 * 1024;
        }

        // The carried native code runs when the app is the carried one --
        // tapped on the home screen, or arriving with the very same code.
        #[cfg(krate_native_image)]
        let native = match &wasm {
            None => true,
            Some(bytes) => krate_bundle::sha256_hex(bytes) == native::SHA256,
        };
        #[cfg(not(krate_native_image))]
        let native = false;

        let outcome = if native {
            run_native(&config)
        } else {
            let Some(wasm) = wasm else {
                return;
            };
            krate_runtime::Runtime::new(&config).and_then(|runtime| {
                runtime.run_bytes_for_world(&wasm, &config, krate_runtime::RuntimeWorld::Gui)
            })
        };
        match outcome {
            Ok(outcome) => eprintln!("app finished: {outcome:?}"),
            Err(err) => {
                eprintln!("app failed: {err:#}");
                notice(
                    data_root,
                    &format!("{} stopped", manifest.app.name),
                    &format!("{err:#}"),
                );
            }
        }
    }

    #[cfg(krate_native_image)]
    fn run_native(
        config: &krate_runtime::Config,
    ) -> krate_runtime::Result<krate_runtime::RunOutcome> {
        let runtime = krate_runtime::Runtime::new_for_native_image(config)?;
        let started = std::time::Instant::now();
        // SAFETY: the image was made by this Krate's precompile_native_image
        // and sits in this binary's signed, executable text for the life of
        // the process.
        let loaded = unsafe { runtime.load_native_image(&native::IMAGE.0) }?;
        eprintln!(
            "native image loaded in {:.1} ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
        runtime.run_loaded_for_world(&loaded, config, krate_runtime::RuntimeWorld::Gui)
    }

    #[cfg(not(krate_native_image))]
    fn run_native(
        _config: &krate_runtime::Config,
    ) -> krate_runtime::Result<krate_runtime::RunOutcome> {
        unreachable!("no native app in this build")
    }

    /// What the app may use, asked the way iOS asks: one native alert
    /// naming the app and every reason it gave. Some(true) allows all,
    /// Some(false) only what the app cannot run without, None opens
    /// nothing. An app that asks for nothing privileged is not asked.
    fn ask_permission(manifest: &krate_manifest::Manifest) -> Option<bool> {
        if manifest.capabilities.is_empty() {
            return Some(true);
        }
        let title = format!("\u{201c}{}\u{201d} would like to", manifest.app.name);
        let mut reasons: Vec<String> = Vec::new();
        for request in &manifest.capabilities {
            let line = request.rationale.trim();
            if !line.is_empty() && !reasons.iter().any(|r| r == line) {
                reasons.push(line.to_string());
            }
        }
        let message = reasons
            .iter()
            .map(|r| format!("\u{2022} {r}"))
            .collect::<Vec<_>>()
            .join("\n");
        let optional = manifest.capabilities.iter().any(|c| !c.required);

        let (tx, rx) = std::sync::mpsc::channel::<Option<bool>>();
        dispatch2::DispatchQueue::main().exec_async(move || {
            present_alert(&title, &message, optional, tx);
        });
        rx.recv().ok().flatten()
    }

    /// Present the permission alert over whatever is on screen, in a window
    /// of its own, and send the answer when a button is tapped.
    fn present_alert(
        title: &str,
        message: &str,
        optional: bool,
        tx: std::sync::mpsc::Sender<Option<bool>>,
    ) {
        let Some(mtm) = MainThreadMarker::new() else {
            let _ = tx.send(None);
            return;
        };
        let Some(window) = overlay_window(mtm) else {
            let _ = tx.send(None);
            return;
        };
        let Some(root) = window.rootViewController() else {
            let _ = tx.send(None);
            return;
        };
        let alert = UIAlertController::alertControllerWithTitle_message_preferredStyle(
            Some(&NSString::from_str(title)),
            Some(&NSString::from_str(message)),
            UIAlertControllerStyle::Alert,
            mtm,
        );
        let action = |label: &str, style: UIAlertActionStyle, answer: Option<bool>| {
            let tx = tx.clone();
            let window = window.clone();
            let handler = block2::RcBlock::new(move |_: std::ptr::NonNull<UIAlertAction>| {
                window.setHidden(true);
                let _ = tx.send(answer);
            });
            UIAlertAction::actionWithTitle_style_handler(
                Some(&NSString::from_str(label)),
                style,
                Some(&handler),
                mtm,
            )
        };
        let allow = action("Allow", UIAlertActionStyle::Default, Some(true));
        alert.addAction(&allow);
        if optional {
            alert.addAction(&action(
                "Allow Only What It Needs",
                UIAlertActionStyle::Default,
                Some(false),
            ));
        }
        alert.addAction(&action(
            "Don\u{2019}t Allow",
            UIAlertActionStyle::Cancel,
            None,
        ));
        alert.setPreferredAction(Some(&allow));
        root.presentViewController_animated_completion(&alert, true, None);
    }

    /// Run the wall guest -- Krate's own trusted screens -- and return what
    /// it answered.
    fn run_wall(data_root: &Path, args: Vec<String>) -> Option<String> {
        run_trusted(WALL_WASM, WALL_MANIFEST, &data_root.join("wall"), args)
    }

    /// Run one of Krate's own screens (the wall, the home screen) with
    /// everything its manifest declares, in `sandbox`, and return what it
    /// printed.
    fn run_trusted(
        wasm: &[u8],
        manifest: &str,
        sandbox: &Path,
        args: Vec<String>,
    ) -> Option<String> {
        let manifest = krate_manifest::Manifest::parse(manifest).ok()?;
        let policy = krate_policy::SessionPolicy::allow_all_declared(&manifest).ok()?;
        let _ = std::fs::create_dir_all(sandbox);
        let config = krate_runtime::Config {
            session_policy: policy,
            sandbox_root: sandbox.to_path_buf(),
            app_args: args,
            phase3_ui_mode: krate_runtime::phase3_ui::Phase3HostUiMode::NativePrototype,
            ..Default::default()
        };
        let runtime = krate_runtime::Runtime::new(&config).ok()?;
        let (_, stdout) = runtime
            .run_bytes_captured_for_world(wasm, &config, krate_runtime::RuntimeWorld::Gui)
            .map_err(|err| eprintln!("{} failed: {err:#}", manifest.app.name))
            .ok()?;
        Some(String::from_utf8_lossy(&stdout).into_owned())
    }

    /// Tell the person why something did not open, and wait for Close.
    fn notice(data_root: &Path, title: &str, message: &str) {
        let _ = run_wall(
            data_root,
            vec![
                "wall:notice".to_string(),
                title.replace(['\n', '\0'], " "),
                message.replace(['\n', '\0'], " "),
            ],
        );
    }

    /// `memory.budget:<MiB>`, if the app declared it. precompile_native_image
    /// reads it the same way: it is part of a native image's memory layout.
    fn memory_budget_mib(manifest: &krate_manifest::Manifest) -> Option<u64> {
        manifest
            .capabilities
            .iter()
            .find_map(|c| c.cap.strip_prefix("memory.budget:")?.parse::<u64>().ok())
    }

    /// App ids become directory names; nothing an id says may escape the
    /// apps folder.
    fn sanitize(id: &str) -> String {
        id.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }
}

#[cfg(target_os = "ios")]
mod hub;

#[cfg(target_os = "ios")]
fn main() {
    player::main();
}

#[cfg(not(target_os = "ios"))]
fn main() {
    eprintln!("the Krate iOS player only runs on iOS");
}
