//! A web page inside an app (krate:ui/web-page, IC-927), through the player
//! activity's Web.java: a WebView in a panel window over the app at the box
//! it laid out.
//!
//! Where a page may go is Krate's one rule,
//! `krate_adapter_common::net::web_page_allowed`, and Web.java asks it here
//! for every navigation and every page start -- through a native method this
//! module registers, because NativeActivity loads Krate's library itself and
//! Java would never find it by name. The hosts each page was granted live
//! here, beside the rule.

#[cfg(target_os = "android")]
mod real {
    use crate::java::{string, with_activity};
    use jni::objects::{JClass, JIntArray, JObject, JObjectArray, JString, JValue};
    use jni::sys::{jlong, jstring};
    use jni::{JNIEnv, NativeMethod};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// Each open page's granted hosts and ports, and its screen's scale, by
    /// handle.
    type Held = (Vec<(String, String)>, f32);
    static GRANTED: Mutex<BTreeMap<u64, Held>> = Mutex::new(BTreeMap::new());

    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

    /// Whether Web.allowed is wired to [`allowed`].
    static REGISTERED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

    /// Web.allowed(page, address): null when the page may go there, else
    /// why not. Runs on the UI thread, for every navigation.
    extern "system" fn allowed<'local>(
        mut env: JNIEnv<'local>,
        _class: JClass<'local>,
        page: jlong,
        address: JString<'local>,
    ) -> jstring {
        let address: String = match env.get_string(&address) {
            Ok(text) => text.into(),
            Err(_) => String::new(),
        };
        let verdict = GRANTED
            .lock()
            .ok()
            .and_then(|granted| granted.get(&(page as u64)).map(|held| held.0.clone()))
            .map_or_else(
                || Err("this page is closed".to_string()),
                |granted| krate_adapter_common::net::web_page_allowed(&address, &granted),
            );
        match verdict {
            Ok(()) => std::ptr::null_mut(),
            Err(why) => env
                .new_string(why)
                .map(JString::into_raw)
                .unwrap_or(std::ptr::null_mut()),
        }
    }

    /// Give Web.java its native judge, once.
    fn register() -> bool {
        *REGISTERED.get_or_init(|| {
            with_activity("wiring the web page's judge", |env, activity| {
                let class = env
                    .call_method(activity, "krateWebClass", "()Ljava/lang/Class;", &[])?
                    .l()?;
                let class = JClass::from(class);
                env.register_native_methods(
                    &class,
                    &[NativeMethod {
                        name: "allowed".into(),
                        sig: "(JLjava/lang/String;)Ljava/lang/String;".into(),
                        fn_ptr: allowed as *mut std::ffi::c_void,
                    }],
                )?;
                Ok(true)
            })
            .unwrap_or(false)
        })
    }

    /// A box in the app's points as screen pixels.
    fn pixels(frame: (f32, f32, f32, f32), scale: f32) -> [i32; 4] {
        [
            (frame.0 * scale).round() as i32,
            (frame.1 * scale).round() as i32,
            (frame.2 * scale).round() as i32,
            (frame.3 * scale).round() as i32,
        ]
    }

    fn int_array<'a>(env: &mut JNIEnv<'a>, values: [i32; 4]) -> jni::errors::Result<JIntArray<'a>> {
        let array = env.new_int_array(4)?;
        env.set_int_array_region(&array, 0, &values)?;
        Ok(array)
    }

    /// Show `address` at `frame` (the app's points), going only to `granted`.
    pub fn open(
        address: &str,
        frame: (f32, f32, f32, f32),
        scale: f32,
        granted: Vec<(String, String)>,
    ) -> Result<u64, String> {
        // The runtime judged the first address; judged again here, so this
        // door holds alone.
        krate_adapter_common::net::web_page_allowed(address, &granted)?;
        if !register() {
            return Err("this player cannot check where a page goes".to_string());
        }
        let page = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        GRANTED
            .lock()
            .map_err(|_| "the web pages are not answering".to_string())?
            .insert(page, (granted, scale));
        let opened = with_activity("opening a web page", |env, activity| {
            let address = env.new_string(address)?;
            let place = int_array(env, pixels(frame, scale))?;
            env.call_method(
                activity,
                "krateWebOpen",
                "(JLjava/lang/String;[I)V",
                &[
                    JValue::Long(page as i64),
                    JValue::Object(&address),
                    JValue::Object(&place),
                ],
            )?;
            Ok(())
        });
        if opened.is_none() {
            forget(page);
            return Err("the web view could not be reached".to_string());
        }
        Ok(page)
    }

    fn forget(page: u64) {
        if let Ok(mut granted) = GRANTED.lock() {
            granted.remove(&page);
        }
    }

    pub fn place(page: u64, frame: (f32, f32, f32, f32)) {
        let Some(scale) = GRANTED
            .lock()
            .ok()
            .and_then(|held| held.get(&page).map(|held| held.1))
        else {
            return;
        };
        with_activity("placing a web page", |env, activity| {
            let place = int_array(env, pixels(frame, scale))?;
            env.call_method(
                activity,
                "krateWebPlace",
                "(J[I)V",
                &[JValue::Long(page as i64), JValue::Object(&place)],
            )?;
            Ok(())
        });
    }

    pub fn go(page: u64, address: &str) {
        with_activity("sending a web page on", |env, activity| {
            let address = env.new_string(address)?;
            env.call_method(
                activity,
                "krateWebGo",
                "(JLjava/lang/String;)V",
                &[JValue::Long(page as i64), JValue::Object(&address)],
            )?;
            Ok(())
        });
    }

    pub fn back(page: u64) {
        with_activity("sending a web page back", |env, activity| {
            env.call_method(
                activity,
                "krateWebBack",
                "(J)V",
                &[JValue::Long(page as i64)],
            )?;
            Ok(())
        });
    }

    pub fn state(page: u64) -> Option<krate_adapter_common::ui::WebPageState> {
        with_activity("reading a web page", |env, activity| {
            let all = env
                .call_method(
                    activity,
                    "krateWebState",
                    "(J)[Ljava/lang/String;",
                    &[JValue::Long(page as i64)],
                )?
                .l()?;
            if all.is_null() {
                return Ok(None);
            }
            let all = JObjectArray::from(all);
            let mut field = |at: i32| -> jni::errors::Result<String> {
                let item: JObject = env.get_object_array_element(&all, at)?;
                Ok(string(env, item)?.unwrap_or_default())
            };
            Ok(Some(krate_adapter_common::ui::WebPageState {
                address: field(0)?,
                title: field(1)?,
                loading: field(2)? == "1",
                problem: field(3)?,
            }))
        })
        .flatten()
    }

    pub fn close(page: u64) {
        forget(page);
        with_activity("closing a web page", |env, activity| {
            env.call_method(
                activity,
                "krateWebClose",
                "(J)V",
                &[JValue::Long(page as i64)],
            )?;
            Ok(())
        });
    }

    /// The app is gone: every page it opened goes with it.
    pub fn close_all() {
        let any = GRANTED.lock().map(|mut g| {
            let any = !g.is_empty();
            g.clear();
            any
        });
        if any.unwrap_or(true) {
            with_activity("closing the web pages", |env, activity| {
                env.call_method(activity, "krateWebCloseAll", "()V", &[])?;
                Ok(())
            });
        }
    }
}

#[cfg(target_os = "android")]
pub use real::*;

#[cfg(not(target_os = "android"))]
mod stub {
    /// No web view off Android.
    pub fn open(
        _address: &str,
        _frame: (f32, f32, f32, f32),
        _scale: f32,
        _granted: Vec<(String, String)>,
    ) -> Result<u64, String> {
        Err("this device cannot show a web page inside an app yet".to_string())
    }

    pub fn place(_page: u64, _frame: (f32, f32, f32, f32)) {}

    pub fn go(_page: u64, _address: &str) {}

    pub fn back(_page: u64) {}

    pub fn state(_page: u64) -> Option<krate_adapter_common::ui::WebPageState> {
        None
    }

    pub fn close(_page: u64) {}

    pub fn close_all() {}
}

#[cfg(not(target_os = "android"))]
pub use stub::*;
