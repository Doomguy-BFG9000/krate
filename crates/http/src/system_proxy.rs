//! The proxy the operating system is set to use (IC-949 N).
//!
//! A company that routes traffic through a proxy usually sets it where the
//! browser reads it -- System Settings on a Mac (often pushed by a
//! configuration profile), Internet Options on Windows (often pushed by
//! Group Policy) -- not in environment variables, which a double-clicked app
//! never sees. So with no proxy variable set, Krate reads the system's.
//!
//! What is read: the static proxy for http and https and the list of hosts
//! that bypass it. What is not: an automatic configuration script (PAC) or
//! auto-discovery (WPAD). Those need a script evaluated per URL; a machine
//! that relies on one names its proxy in the Krate policy instead
//! (`[network] proxy`).
//!
//! Each OS call is a thin read feeding a pure parser, so the parsing is
//! tested on every OS and the call itself is the only untested line.

/// The system's static proxy settings, in the forms the rest of this crate
/// reads: `http://host:port` and a comma-separated `NO_PROXY` list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemProxy {
    pub http: Option<String>,
    pub https: Option<String>,
    pub bypass: Option<String>,
}

impl SystemProxy {
    fn is_empty(&self) -> bool {
        self.http.is_none() && self.https.is_none()
    }
}

/// The system's settings, read once per process. `None` when the system
/// names no static proxy.
pub fn system_proxy() -> Option<&'static SystemProxy> {
    static PROXY: std::sync::OnceLock<Option<SystemProxy>> = std::sync::OnceLock::new();
    PROXY
        .get_or_init(|| Some(read()).filter(|proxy| !proxy.is_empty()))
        .as_ref()
}

#[cfg(target_os = "macos")]
fn read() -> SystemProxy {
    use system_configuration::core_foundation::array::CFArray;
    use system_configuration::core_foundation::base::{CFType, TCFType};
    use system_configuration::core_foundation::number::CFNumber;
    use system_configuration::core_foundation::string::CFString;
    use system_configuration::dynamic_store::SCDynamicStoreBuilder;

    let Some(store) = SCDynamicStoreBuilder::new("krate").build() else {
        return SystemProxy::default();
    };
    let Some(settings) = store.get_proxies() else {
        return SystemProxy::default();
    };
    let get = |key: &str| settings.find(CFString::new(key));
    from_mac_settings(
        |key| {
            let item = get(key)?;
            if let Some(text) = item.downcast::<CFString>() {
                return Some(text.to_string());
            }
            item.downcast::<CFNumber>()
                .and_then(|number| number.to_i64())
                .map(|number| number.to_string())
        },
        get("ExceptionsList")
            .and_then(|item| item.downcast::<CFArray<*const std::ffi::c_void>>())
            .map(|list| {
                list.get_all_values()
                    .into_iter()
                    .filter_map(|entry| {
                        // SAFETY: every value of a CFArray is a CF object it
                        // retains for as long as `list` lives; wrapping under
                        // the get rule takes our own reference.
                        let entry = unsafe { CFType::wrap_under_get_rule(entry as _) };
                        entry.downcast::<CFString>().map(|s| s.to_string())
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

#[cfg(windows)]
fn read() -> SystemProxy {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::Networking::WinHttp::{
        WinHttpGetIEProxyConfigForCurrentUser, WINHTTP_CURRENT_USER_IE_PROXY_CONFIG,
    };

    /// A string WinHTTP allocated, read and freed.
    unsafe fn take(text: *mut u16) -> Option<String> {
        if text.is_null() {
            return None;
        }
        let mut len = 0;
        while *text.add(len) != 0 {
            len += 1;
        }
        let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        GlobalFree(text.cast());
        Some(value)
    }

    let mut config = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG {
        fAutoDetect: 0,
        lpszAutoConfigUrl: std::ptr::null_mut(),
        lpszProxy: std::ptr::null_mut(),
        lpszProxyBypass: std::ptr::null_mut(),
    };
    // SAFETY: `config` is a valid out-parameter; every string WinHTTP hands
    // back is read once and freed with GlobalFree, as its documentation says.
    unsafe {
        if WinHttpGetIEProxyConfigForCurrentUser(&mut config) == 0 {
            return SystemProxy::default();
        }
        let _ = take(config.lpszAutoConfigUrl);
        let proxy = take(config.lpszProxy);
        let bypass = take(config.lpszProxyBypass);
        from_windows_settings(proxy.as_deref(), bypass.as_deref())
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn read() -> SystemProxy {
    // Linux has no one system setting every program reads; the desktop's
    // own settings are exported as the proxy variables, which come first.
    SystemProxy::default()
}

/// macOS's proxy dictionary (`scutil --proxy` shows it) as [`SystemProxy`].
/// `get` reads a key as text; `exceptions` is `ExceptionsList`.
pub fn from_mac_settings(
    get: impl Fn(&str) -> Option<String>,
    exceptions: Vec<String>,
) -> SystemProxy {
    let on = |key: &str| get(key).is_some_and(|v| v.trim() == "1");
    let proxy = |enable: &str, host: &str, port: &str| {
        if !on(enable) {
            return None;
        }
        let host = get(host).filter(|h| !h.trim().is_empty())?;
        let port = get(port)
            .and_then(|p| p.trim().parse::<u16>().ok())
            .unwrap_or(80);
        Some(format!("http://{}:{port}", host.trim()))
    };
    let mut bypass = exceptions;
    if on("ExcludeSimpleHostnames") {
        bypass.push("<local>".into());
    }
    SystemProxy {
        http: proxy("HTTPEnable", "HTTPProxy", "HTTPPort"),
        https: proxy("HTTPSEnable", "HTTPSProxy", "HTTPSPort"),
        bypass: join(bypass),
    }
}

/// Windows' Internet Options proxy (`lpszProxy`, `lpszProxyBypass`) as
/// [`SystemProxy`]. The proxy is either one `host:port` for every scheme or
/// a list like `http=host:80;https=host:443`.
pub fn from_windows_settings(proxy: Option<&str>, bypass: Option<&str>) -> SystemProxy {
    let mut out = SystemProxy::default();
    let with_scheme = |address: &str| {
        let address = address.trim();
        if address.contains("://") {
            address.to_string()
        } else {
            format!("http://{address}")
        }
    };
    for part in proxy
        .unwrap_or("")
        .split([';', ' '])
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        match part.split_once('=') {
            Some((scheme, address)) if !address.trim().is_empty() => {
                match scheme.trim().to_ascii_lowercase().as_str() {
                    "http" => out.http = Some(with_scheme(address)),
                    "https" => out.https = Some(with_scheme(address)),
                    _ => {}
                }
            }
            Some(_) => {}
            None => {
                out.http = Some(with_scheme(part));
                out.https = Some(with_scheme(part));
            }
        }
    }
    out.bypass = join(
        bypass
            .unwrap_or("")
            .split([';', ',', ' '])
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(str::to_string)
            .collect(),
    );
    out
}

fn join(entries: Vec<String>) -> Option<String> {
    let entries: Vec<String> = entries
        .into_iter()
        .filter(|e| !e.trim().is_empty())
        .collect();
    (!entries.is_empty()).then(|| entries.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn a_mac_with_both_proxies_and_exceptions() {
        let keys: HashMap<&str, &str> = [
            ("HTTPEnable", "1"),
            ("HTTPProxy", "proxy.acme.com"),
            ("HTTPPort", "8080"),
            ("HTTPSEnable", "1"),
            ("HTTPSProxy", "secure.acme.com"),
            ("HTTPSPort", "3128"),
            ("ExcludeSimpleHostnames", "1"),
        ]
        .into_iter()
        .collect();
        let got = from_mac_settings(
            |k| keys.get(k).map(|v| v.to_string()),
            vec!["*.local".into(), "169.254/16".into()],
        );
        assert_eq!(got.http.as_deref(), Some("http://proxy.acme.com:8080"));
        assert_eq!(got.https.as_deref(), Some("http://secure.acme.com:3128"));
        assert_eq!(got.bypass.as_deref(), Some("*.local,169.254/16,<local>"));
    }

    #[test]
    fn a_mac_proxy_that_is_switched_off_is_not_used() {
        let keys: HashMap<&str, &str> = [
            ("HTTPSEnable", "0"),
            ("HTTPSProxy", "secure.acme.com"),
            ("HTTPSPort", "3128"),
        ]
        .into_iter()
        .collect();
        let got = from_mac_settings(|k| keys.get(k).map(|v| v.to_string()), vec![]);
        assert_eq!(got, SystemProxy::default());
        assert!(got.is_empty());
    }

    #[test]
    fn windows_one_proxy_for_everything() {
        let got = from_windows_settings(Some("proxy.acme.com:8080"), Some("<local>;*.acme.com"));
        assert_eq!(got.http.as_deref(), Some("http://proxy.acme.com:8080"));
        assert_eq!(got.https.as_deref(), Some("http://proxy.acme.com:8080"));
        assert_eq!(got.bypass.as_deref(), Some("<local>,*.acme.com"));
    }

    #[test]
    fn reading_this_computers_settings_never_fails() {
        // The live OS call, on whatever this computer is set to. What it
        // finds depends on the machine; that it answers is the test.
        let _ = read();
        let _ = system_proxy();
    }

    #[test]
    fn windows_a_proxy_per_scheme() {
        let got = from_windows_settings(
            Some("http=a.acme.com:80;https=b.acme.com:443;ftp=c:21"),
            None,
        );
        assert_eq!(got.http.as_deref(), Some("http://a.acme.com:80"));
        assert_eq!(got.https.as_deref(), Some("http://b.acme.com:443"));
        assert_eq!(got.bypass, None);
        assert_eq!(from_windows_settings(None, None), SystemProxy::default());
    }
}
