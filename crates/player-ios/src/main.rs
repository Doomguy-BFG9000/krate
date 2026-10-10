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
                // Krate stays alive in the background now, so this usually
                // arrives with the home screen or an app already open: close
                // it, and the player opens the file next. At a cold start
                // nothing is running yet and the player reads OPENED first.
                if self.ivars().started.get() {
                    krate_adapter_ios::uikit_native::close_for_opened_file();
                }
                objc2::runtime::Bool::YES
            }

            /// Leaving Krate (another app, the home screen, the lock) used
            /// to end the process: whatever the person had open, playing or
            /// half-typed was gone when they came back. Krate now waits in
            /// the background as a phone's own apps do, drawing nothing.
            #[unsafe(method(applicationDidEnterBackground:))]
            fn did_enter_background(&self, _application: &UIApplication) {
                krate_adapter_ios::uikit_native::set_background(true);
            }

            #[unsafe(method(applicationWillEnterForeground:))]
            fn will_enter_foreground(&self, _application: &UIApplication) {
                krate_adapter_ios::uikit_native::set_background(false);
            }

            #[unsafe(method(applicationDidBecomeActive:))]
            fn did_become_active(&self, _application: &UIApplication) {
                // Back on screen, however it got there: iOS does not promise
                // will-enter-foreground for every return, and a background
                // flag left set draws nothing ever again.
                krate_adapter_ios::uikit_native::set_background(false);
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

    /// Krate's sound is media the person chose to start: it plays with the
    /// phone's silent switch on, as a music app's does. Left alone, iOS's
    /// default session is silenced by the switch -- Shelf played every note
    /// into a muted session and the person heard nothing.
    fn audio_session() {
        // SAFETY: the shared session exists for the life of the process;
        // setting a category and activating it are ordinary session calls.
        unsafe {
            let session = objc2_avf_audio::AVAudioSession::sharedInstance();
            if let Some(playback) = objc2_avf_audio::AVAudioSessionCategoryPlayback {
                if let Err(why) = session.setCategory_error(playback) {
                    eprintln!("krate: audio session category refused: {why:?}");
                }
            }
            let _ = session.setActive_error(true);
        }
    }

    fn run_player() {
        audio_session();
        let data_root = std::env::var("HOME")
            .map(|home| PathBuf::from(home).join("Documents"))
            .unwrap_or_else(|_| PathBuf::from("."));
        let library = data_root.join("Library");
        let _ = std::fs::create_dir_all(&library);

        krate_adapter_ios::uikit_native::show_loading("Krate", "");
        let mut next = None;
        loop {
            // A file opened from Messages, Mail or Files -- at start, or
            // while something else was on screen -- comes first.
            if let Some(path) = OPENED.lock().expect("opened lock").take() {
                if let Some(app) = receive(&path, &library, &data_root) {
                    next = Some(app);
                }
            }
            let app = match next.take() {
                Some(app) => app,
                None => match home(&library, &data_root) {
                    Some(app) => app,
                    // The home screen closed because a file was opened: go
                    // round and open it. Otherwise Krate's home was closed.
                    None if OPENED.lock().expect("opened lock").is_some() => continue,
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
            remember(library, &into, &manifest.app.name);
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

    /// The library's index: one line per kept app -- file name, app name,
    /// SHA-256, size -- so the home screen lists the library without
    /// unpacking every app to read its name each time it is drawn.
    fn remember(library: &Path, file: &Path, name: &str) {
        let Ok(bytes) = std::fs::read(file) else {
            return;
        };
        let file_name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let index = library.join("index.tsv");
        let mut lines: Vec<String> = std::fs::read_to_string(&index)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.split('\t').next() != Some(file_name.as_str()))
            .map(str::to_string)
            .collect();
        lines.push(format!(
            "{file_name}\t{}\t{}\t{}",
            field(name),
            krate_bundle::sha256_hex(&bytes),
            bytes.len()
        ));
        let _ = std::fs::write(index, lines.join("\n") + "\n");
    }

    /// The library's apps, newest first: (file, name, sha, size). Apps kept
    /// before the index existed are read once and added to it.
    fn library_apps(library: &Path) -> Vec<(PathBuf, String, String, u64)> {
        let index = std::fs::read_to_string(library.join("index.tsv")).unwrap_or_default();
        let mut known: std::collections::HashMap<String, (String, String, u64)> = index
            .lines()
            .filter_map(|l| {
                let f: Vec<&str> = l.split('\t').collect();
                (f.len() == 4).then(|| {
                    (
                        f[0].to_string(),
                        (
                            f[1].to_string(),
                            f[2].to_string(),
                            f[3].parse().unwrap_or(0),
                        ),
                    )
                })
            })
            .collect();
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(library)
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
        files.sort_by(|a, b| b.0.cmp(&a.0));
        let mut out = Vec::new();
        for (_, path) in files {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if !known.get(&name).is_some_and(|k| k.2 == size) {
                let app_name = krate_bundle::open(&path)
                    .ok()
                    .and_then(|open| std::fs::read_to_string(open.manifest_path()).ok())
                    .and_then(|text| krate_manifest::Manifest::parse(&text).ok())
                    .map(|manifest| manifest.app.name);
                let Some(app_name) = app_name else {
                    continue;
                };
                remember(library, &path, &app_name);
                let sha = std::fs::read(&path)
                    .map(|b| krate_bundle::sha256_hex(&b))
                    .unwrap_or_default();
                known.insert(name.clone(), (app_name, sha, size));
            }
            if let Some((app_name, sha, size)) = known.get(&name).cloned() {
                out.push((path, app_name, sha, size));
            }
        }
        out
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
        // The hub's catalogue as last fetched, refreshed in the background
        // for next time -- coming back from an app waited seconds on the
        // network. Only the very first visit, with nothing cached, waits.
        if crate::hub::catalogue(&ui).is_empty() {
            krate_adapter_ios::uikit_native::show_loading("Krate", "Loading apps");
            crate::hub::refresh(&ui, &art_dir);
        } else {
            static REFRESHED: std::sync::Once = std::sync::Once::new();
            REFRESHED.call_once(|| {
                let (ui, art_dir) = (ui.clone(), art_dir.clone());
                std::thread::spawn(move || crate::hub::refresh(&ui, &art_dir));
            });
        }
        loop {
            let hub_apps = crate::hub::catalogue(&ui);
            let hub_art: std::collections::HashMap<String, String> = hub_apps
                .iter()
                .filter_map(|app| Some((app.id.clone(), crate::hub::cached_art(app, &art_dir)?)))
                .collect();

            let mut apps: Vec<App> = Vec::new();
            let mut rows: Vec<String> = Vec::new();
            #[cfg(krate_native_image)]
            if let Ok(manifest) = krate_manifest::Manifest::parse(native::MANIFEST) {
                apps.push(App::Carried);
                let preview = preview_name(&App::Carried);
                let art = if art_dir.join(&preview).exists() {
                    preview.as_str()
                } else {
                    "-"
                };
                rows.push(format!(
                    "{}\tBuilt in \u{00b7} Runs at full speed\t{art}",
                    field(&manifest.app.name)
                ));
            }
            for (path, name, sha, size) in library_apps(library) {
                // The app as it really looked the last time it ran on this
                // phone; else, for an app from the hub, its hub picture (the
                // hub names apps by the SHA-256 of their bytes).
                let preview = preview_name(&App::Received(path.clone()));
                let art = if art_dir.join(&preview).exists() {
                    preview.as_str()
                } else {
                    hub_art.get(&sha).map(String::as_str).unwrap_or("-")
                };
                rows.push(format!(
                    "{}\tReceived \u{00b7} {}\t{art}",
                    field(&name),
                    human_size(size)
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
                let name = hub_apps
                    .iter()
                    .find(|app| app.id == id)
                    .map(|app| app.name.clone())
                    .unwrap_or_else(|| "The app".to_string());
                krate_adapter_ios::uikit_native::show_loading(&name, "Getting it from the hub");
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

    /// The file an app's preview is kept in, in the home screen's art folder:
    /// one per library file, so a newer copy of an app replaces its preview.
    /// (`screen-`, not the first builds' `preview-`: those could hold a
    /// picture of the Library itself and are left unread.)
    fn preview_name(app: &App) -> String {
        match app {
            #[cfg(krate_native_image)]
            App::Carried => "screen-carried.rgba".to_string(),
            App::Received(path) => format!(
                "screen-{}.rgba",
                sanitize(
                    &path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default()
                )
            ),
        }
    }

    /// Ask, then run. Every open asks: the answer is never remembered.
    fn launch(app: App, data_root: &Path) {
        let preview_path = data_root
            .join("home-ui")
            .join("art")
            .join(preview_name(&app));
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

        // The line under the app's name on the permission sheet.
        let detail = match &app {
            #[cfg(krate_native_image)]
            App::Carried => format!("Version {} \u{00b7} Built in", manifest.app.version),
            App::Received(path) => match std::fs::metadata(path) {
                Ok(meta) => format!(
                    "Version {} \u{00b7} {}",
                    manifest.app.version,
                    human_size(meta.len())
                ),
                Err(_) => format!("Version {}", manifest.app.version),
            },
        };
        let Some(granted) = ask_on_wall(data_root, &manifest, &detail) else {
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
            .filter(|(request, _)| {
                request.required || granted.iter().any(|cap| *cap == request.cap)
            })
            .map(|(_, capability)| capability)
            .collect();

        let sandbox = data_root.join("apps").join(sanitize(&manifest.app.id));
        let _ = std::fs::create_dir_all(&sandbox);
        // The app's key-value store and database: the runtime opens them only
        // when told where, and the phone players never said, so every
        // store.kv and store.sql call was refused and a recipe book made in
        // Studio quit at start. Kept beside the sandbox, not in it, as on a
        // desktop: the app names keys, never files.
        let stores = data_root.join("stores");
        let _ = std::fs::create_dir_all(&stores);
        let store = stores.join(format!("{}.kv", sanitize(&manifest.app.id)));
        let mut config = krate_runtime::Config {
            session_policy: krate_policy::SessionPolicy::from_grants(kept),
            sandbox_root: sandbox,
            app_store_path: Some(store.clone()),
            app_database_path: Some(store.with_extension("sqlite")),
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

        // The cover while the app starts -- the screen turning, an engine
        // loading -- lifting on the app's first frame at the screen's real
        // size; Krate's close capsule over the app, the way back home.
        krate_adapter_ios::uikit_native::show_loading(&manifest.app.name, "Opening");
        krate_adapter_ios::uikit_native::set_app_chrome(true);
        // A picture of the app as it really looks, for its home-screen card,
        // taken a few seconds into its own run (once per run). Asked only
        // now, past the permission sheet: asked earlier, a slow answer on the
        // sheet or a Don't Allow took the picture of Krate's own Library.
        krate_adapter_ios::uikit_native::capture_preview(
            preview_path,
            std::time::Duration::from_millis(4000),
        );
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
        krate_adapter_ios::uikit_native::cancel_preview();
        krate_adapter_ios::uikit_native::set_app_chrome(false);
        // Back home: the cover until Krate's own screen has drawn.
        krate_adapter_ios::uikit_native::show_loading("Krate", "");
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

    /// Ask on Krate's own permission sheet (apps/krate-wall): the app's
    /// name, then each request's capability, its reason and whether the app
    /// needs it. Returns what the person allowed, or None when they did not
    /// open the app. An app that asks for nothing opens without asking.
    fn ask_on_wall(
        data_root: &Path,
        manifest: &krate_manifest::Manifest,
        detail: &str,
    ) -> Option<Vec<String>> {
        if manifest.capabilities.is_empty() {
            return Some(Vec::new());
        }
        // `wall:ask`, the name, then the line under it as its own argument,
        // so a name cannot forge that line.
        let mut args = vec![
            "wall:ask".to_string(),
            field(&manifest.app.name),
            field(detail),
        ];
        for request in &manifest.capabilities {
            args.push(field(&request.cap));
            args.push(field(&request.rationale));
            args.push(if request.required { "1" } else { "0" }.to_string());
        }
        let answer = run_wall(data_root, args)?;
        let caps = answer.trim().strip_prefix("wall:open:")?;
        Some(
            caps.split(',')
                .filter(|cap| !cap.is_empty())
                .map(str::to_string)
                .collect(),
        )
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
