//! The Krate player for Android -- M1 of the mobile plan.
//!
//! One job: open a .krate, show the wall, run the app. Phones receive;
//! desktops make. Three ways in, one flow:
//!
//! - a tapped krate.tech/a/ link (VIEW intent, https) -- fetched from the
//!   hub over TLS;
//! - a tapped .krate file (VIEW intent, content:// or file://) -- read
//!   through the ContentResolver;
//! - a plain launch -- the embedded demo app.
//!
//! Whatever the source, the app never runs before the wall: a trusted
//! sheet (itself a Krate guest whose pixels come from the same renderer)
//! shows every capability line with the app's own rationale, required
//! rows locked on, the rest togglable. The player passes the lines in and
//! reads one decision line back from captured stdout; both sides of that
//! pipe belong to the player, so the sheet cannot be bypassed and cannot
//! be lied to. Deny still opens: whatever was toggled off is simply not
//! in the session policy, and the runtime enforces the difference.

#[cfg(target_os = "android")]
mod intent;

#[cfg(target_os = "android")]
mod player {
    use krate_adapter_android::winit_native::AndroidApp;
    use std::io::Cursor;

    const MAX_BUNDLE_BYTES: usize = 6 * 1024 * 1024;

    // The demo app, for a plain launch with no intent: krate-gram, the
    // modern-UI acceptance test. It goes through the same wall as any
    // downloaded app -- the demo demonstrates the product, and the wall is
    // the product.
    const GRAM_WASM: &[u8] =
        include_bytes!("../../../apps/krate-gram/target/wasm32-wasip1/release/krate_gram.wasm");
    const GRAM_MANIFEST: &str = include_str!("../../../apps/krate-gram/manifest.toml");

    // The wall sheet, a trusted guest. Build apps/krate-wall before the
    // player.
    const WALL_WASM: &[u8] =
        include_bytes!("../../../apps/krate-wall/target/wasm32-wasip1/release/krate_wall.wasm");
    const WALL_MANIFEST: &str = include_str!("../../../apps/krate-wall/manifest.toml");

    #[no_mangle]
    fn android_main(app: AndroidApp) {
        android_logger::init_once(
            android_logger::Config::default().with_max_level(log::LevelFilter::Info),
        );

        krate_adapter_android::winit_native::set_android_app(app.clone());
        let data_root = app
            .internal_data_path()
            .unwrap_or_else(|| std::path::PathBuf::from("."));

        let handles = crate::intent::ActivityHandles {
            vm: app.vm_as_ptr(),
            activity: app.activity_as_ptr(),
        };
        // An app's temporary space is the activity's cache: Rust's
        // temp_dir() is /data/local/tmp on Android, which no app may write,
        // and a save on a phone is written there before it is placed
        // (K-1032). Picks and saves a run left behind go at the next start.
        let cache = crate::intent::cache_dir(handles).unwrap_or_else(|| data_root.join("tmp"));
        for left in ["krate-picked", "krate-saves", "krate-share"] {
            let _ = std::fs::remove_dir_all(cache.join(left));
        }
        let _ = std::fs::create_dir_all(&cache);
        std::env::set_var("TMPDIR", &cache);
        run_player(&data_root, handles);
        // The app is the process: when the flow ends -- back button, close,
        // cancel at the wall -- no blank activity shell may linger. Finish
        // the activity first: Android restarts a top activity whose process
        // died moments after launch, with the same link, so a file that
        // could not be read flickered open and shut forever (K-1015).
        crate::intent::finish(handles);
        std::process::exit(0);
    }

    fn run_player(data_root: &std::path::Path, handles: crate::intent::ActivityHandles) {
        // What was tapped decides what runs.
        let target = crate::intent::view_target(handles);
        log::info!("launch target: {target:?}");

        let manifest_text: String;
        let component: Vec<u8>;
        let assets_root: Option<std::path::PathBuf>;
        // The extracted bundle dir is removed on drop, and the runtime
        // reads assets from it during the run -- it must live to the end.
        let mut _opened_bundle = None;
        // The bundle as it came, kept so a reminder tapped later can open
        // this app again (IC-921): a link may be offline by then, and a
        // file's grant from the app that sent it does not last.
        let mut kept_bytes: Option<Vec<u8>> = None;

        match target {
            Some(uri) => {
                let bytes = if uri.starts_with("https://") || uri.starts_with("http://") {
                    log::info!("fetching {uri}");
                    match fetch(&uri) {
                        Ok(bytes) => bytes,
                        Err(why) => {
                            log::error!("could not fetch the app: {why}");
                            let why =
                                format!("{why}. Check the connection and open the link again.");
                            return notice(data_root, "Could not download this app", &why);
                        }
                    }
                } else {
                    log::info!("opening {uri}");
                    match crate::intent::read_uri(handles, &uri, MAX_BUNDLE_BYTES) {
                        Ok(bytes) => bytes,
                        Err(why) => {
                            log::error!("could not read the file: {why}");
                            return notice(data_root, "Could not open this file", &why);
                        }
                    }
                };
                kept_bytes = Some(bytes.clone());
                let open = match krate_bundle::open_reader(Cursor::new(bytes)) {
                    Ok(open) => open,
                    Err(err) => {
                        log::error!("that is not a Krate app: {err:#}");
                        // A Krate too old for the app says so in words a person
                        // can act on; anything else is a damaged or wrong file.
                        let why = match err {
                            krate_bundle::BundleError::NeedsNewerKrate { .. } => format!("{err}"),
                            _ => "This file is not a Krate app, or it was damaged on the way. Ask for it again.".to_string(),
                        };
                        return notice(data_root, "This app did not open", &why);
                    }
                };
                manifest_text = std::fs::read_to_string(open.manifest_path()).unwrap_or_default();
                component = match std::fs::read(open.component_path()) {
                    Ok(bytes) => bytes,
                    Err(err) => {
                        log::error!("bundle component unreadable: {err}");
                        return;
                    }
                };
                assets_root = open.assets_path().map(|p| p.to_path_buf());
                _opened_bundle = Some(open);
            }
            None => {
                manifest_text = GRAM_MANIFEST.to_string();
                component = GRAM_WASM.to_vec();
                assets_root = None;
            }
        }

        let manifest = match krate_manifest::Manifest::parse(&manifest_text) {
            Ok(manifest) => manifest,
            Err(err) => {
                log::error!("manifest failed to parse: {err:#}");
                return;
            }
        };

        // The wall, before anything else happens.
        let granted = match show_wall(&manifest, data_root) {
            Some(granted) => granted,
            None => {
                log::info!("the person said no; nothing runs");
                return;
            }
        };

        // The policy is exactly what survived the wall. declared_capabilities
        // parses the same request list the wall displayed, in the same
        // order, so filtering by the displayed cap string cannot drift.
        let declared = match manifest.declared_capabilities() {
            Ok(declared) => declared,
            Err(err) => {
                log::error!("capabilities failed to parse: {err:#}");
                return;
            }
        };
        let kept: Vec<_> = manifest
            .capabilities
            .iter()
            .zip(declared)
            .filter(|(request, _)| granted.iter().any(|g| g == &request.cap))
            .map(|(_, capability)| capability)
            .collect();
        let policy = krate_policy::SessionPolicy::from_grants(kept);

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
        let config = krate_runtime::Config {
            session_policy: policy,
            sandbox_root: sandbox,
            app_store_path: Some(store.clone()),
            app_database_path: Some(store.with_extension("sqlite")),
            bundle_assets_root: assets_root,
            // The name the person agreed to on the wall: a notification's
            // sender line, never text the app picks per call.
            app_name: Some(manifest.app.name.clone()),
            phase3_ui_mode: krate_runtime::phase3_ui::Phase3HostUiMode::NativePrototype,
            ..Default::default()
        };
        // Which app runs, for its reminders: their ids carry its id, and a
        // tapped one opens the copy kept here.
        let opens = kept_bytes
            .and_then(|bytes| {
                let kept = data_root.join("kept");
                let _ = std::fs::create_dir_all(&kept);
                let path = kept.join(format!("{}.krate", sanitize(&manifest.app.id)));
                std::fs::write(&path, bytes).ok()?;
                Some(format!("file://{}", path.display()))
            })
            .unwrap_or_default();
        krate_adapter_android::reminders::set_current_app(Some((manifest.app.id.clone(), opens)));
        let runtime = match krate_runtime::Runtime::new(&config) {
            Ok(runtime) => runtime,
            Err(err) => {
                log::error!("engine failed to start: {err:#}");
                return;
            }
        };
        match runtime.run_bytes_for_world(&component, &config, krate_runtime::RuntimeWorld::Auto) {
            Ok(outcome) => log::info!("app finished: {outcome:?}"),
            Err(err) => {
                log::error!("app failed: {err:#}");
                notice(
                    data_root,
                    &format!("{} stopped", manifest.app.name),
                    &format!("{err:#}"),
                );
            }
        }
    }

    /// Run the wall sheet and return the granted capability names, or None
    /// for cancel. An app that declares nothing privileged shows no wall.
    fn show_wall(
        manifest: &krate_manifest::Manifest,
        data_root: &std::path::Path,
    ) -> Option<Vec<String>> {
        if manifest.capabilities.is_empty() {
            return Some(Vec::new());
        }

        // One argument per field, never one string the wall has to cut up.
        //
        // This used to join the name, capability, rationale and required flag
        // with U+001E and U+001C and hand the wall the result. Nothing checked
        // the manifest strings for those characters, so an app name could
        // carry them and add rows to the wall: an app declaring only ui.window
        // displayed a net.connect request with a convincing reason, and a
        // crafted rationale could flip its own capability from required to
        // optional -- required rows being the ones a person cannot switch off.
        //
        // The runtime rejects any argument containing a newline or a NUL
        // (encode_args_raw), so argument boundaries cannot be forged the way
        // an in-band separator can. Manifest parsing now refuses these
        // characters in displayed fields as well; both locks stay.
        // `wall:ask` first: the line under the name (its version) is its own
        // argument, so a name cannot forge it.
        let mut input = vec![
            "wall:ask".to_string(),
            manifest.app.name.clone(),
            format!("Version {}", manifest.app.version),
        ];
        for request in &manifest.capabilities {
            input.push(request.cap.clone());
            input.push(request.rationale.clone());
            input.push(if request.required { "1" } else { "0" }.to_string());
        }

        let wall_manifest = krate_manifest::Manifest::parse(WALL_MANIFEST).ok()?;
        let policy = krate_policy::SessionPolicy::allow_all_declared(&wall_manifest).ok()?;
        let config = krate_runtime::Config {
            session_policy: policy,
            sandbox_root: data_root.join("wall"),
            app_args: input,
            phase3_ui_mode: krate_runtime::phase3_ui::Phase3HostUiMode::NativePrototype,
            ..Default::default()
        };
        let runtime = krate_runtime::Runtime::new(&config).ok()?;
        let (_, stdout) = runtime
            .run_bytes_captured_for_world(WALL_WASM, &config, krate_runtime::RuntimeWorld::Gui)
            .map_err(|err| log::error!("the wall failed: {err:#}"))
            .ok()?;

        let text = String::from_utf8_lossy(&stdout);
        for line in text.lines() {
            if let Some(list) = line.strip_prefix("wall:open:") {
                return Some(
                    list.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect(),
                );
            }
            if line.trim() == "wall:cancel" {
                return None;
            }
        }
        // No decision line at all -- a crashed or killed wall. Fail closed.
        None
    }

    /// Tell the person why nothing opened, on the wall's trusted card, and
    /// wait for Close. A failure that only reached the log looked, on a
    /// phone, like the app flashing and vanishing.
    fn notice(data_root: &std::path::Path, title: &str, message: &str) {
        // The runtime refuses arguments with newlines; a reason never needs one.
        let message = message.replace(['\n', '\0'], " ");
        let Some(wall_manifest) = krate_manifest::Manifest::parse(WALL_MANIFEST).ok() else {
            return;
        };
        let Ok(policy) = krate_policy::SessionPolicy::allow_all_declared(&wall_manifest) else {
            return;
        };
        let config = krate_runtime::Config {
            session_policy: policy,
            sandbox_root: data_root.join("wall"),
            app_args: vec![
                "wall:notice".to_string(),
                title.replace(['\n', '\0'], " "),
                message,
            ],
            phase3_ui_mode: krate_runtime::phase3_ui::Phase3HostUiMode::NativePrototype,
            ..Default::default()
        };
        if let Ok(runtime) = krate_runtime::Runtime::new(&config) {
            let _ = runtime.run_bytes_captured_for_world(
                WALL_WASM,
                &config,
                krate_runtime::RuntimeWorld::Gui,
            );
        }
    }

    fn fetch(url: &str) -> Result<Vec<u8>, String> {
        let response = ureq::get(url).call().map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        use std::io::Read;
        response
            .into_reader()
            .take(MAX_BUNDLE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > MAX_BUNDLE_BYTES {
            return Err("that download is larger than any Krate app".to_string());
        }
        Ok(bytes)
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
