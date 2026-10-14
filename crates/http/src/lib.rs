//! The one way Krate makes an HTTP(S) client (IC-949 N).
//!
//! Every connection Krate makes -- to the hub, to a builder, to an AI
//! provider, an app's own `net.connect` -- goes through here, so it works
//! on a company network without each call site having to remember how:
//!
//! - **The computer's own trust store.** A company that inspects secure
//!   traffic installs its own root certificate on every laptop through MDM.
//!   A client that trusts only a bundled list of roots refuses every
//!   connection on that network, and the person sees "Krate is broken".
//!   Trusting what the computer trusts is also what a security team
//!   expects: removing a root from the computer removes it from Krate.
//!   Linux adds Mozilla's roots on top, so a fresh machine with no CA bundle
//!   installed still reaches the public internet. The phone players keep the
//!   bundled roots they ship today.
//! - **The proxy settings.** `HTTPS_PROXY` for https, `HTTP_PROXY` for http,
//!   `ALL_PROXY` for either, each also lowercase; `NO_PROXY` lists hosts to
//!   reach directly. Loopback is always reached directly: a proxy cannot
//!   reach this computer's own services, and Krate's local tools live there.
//!
//! ureq reads proxy variables itself, but uses one proxy for every scheme
//! and ignores `NO_PROXY`, so the choice is made here, per URL.

mod system_proxy;
pub use system_proxy::{from_mac_settings, from_windows_settings, system_proxy, SystemProxy};

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// TLS settings that trust the computer's own store. Built once.
pub fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG.get_or_init(|| Arc::new(build_tls_config())).clone()
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn build_tls_config() -> rustls::ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    match platform_verifier(provider.clone()) {
        Ok(verifier) => rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("ring supports the default protocol versions")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth(),
        // The platform could not give us a verifier at all (no store to
        // read). Fall back to the bundled roots rather than refusing every
        // connection: that is what Krate did before, and it still reaches
        // the public internet.
        Err(_) => bundled_roots_config(provider),
    }
}

#[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
fn platform_verifier(
    provider: Arc<rustls::crypto::CryptoProvider>,
) -> Result<rustls_platform_verifier::Verifier, rustls::Error> {
    rustls_platform_verifier::Verifier::new_with_extra_roots(
        webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().cloned(),
        provider,
    )
}

#[cfg(not(any(
    all(unix, not(target_vendor = "apple"), not(target_os = "android")),
    target_os = "android",
    target_os = "ios"
)))]
fn platform_verifier(
    provider: Arc<rustls::crypto::CryptoProvider>,
) -> Result<rustls_platform_verifier::Verifier, rustls::Error> {
    rustls_platform_verifier::Verifier::new(provider)
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn build_tls_config() -> rustls::ClientConfig {
    bundled_roots_config(Arc::new(rustls::crypto::ring::default_provider()))
}

/// What ureq does on its own: Mozilla's roots, compiled in.
fn bundled_roots_config(provider: Arc<rustls::crypto::CryptoProvider>) -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth()
}

/// A client builder for requests to `url`: the computer's trust store and
/// the proxy that applies to this URL. Call sites add their own timeouts.
pub fn builder(url: &str) -> ureq::AgentBuilder {
    let builder = ureq::AgentBuilder::new()
        .tls_config(tls_config())
        // The choice is made here, not by ureq's own reading of the
        // environment (one proxy for every scheme, NO_PROXY ignored).
        .try_proxy_from_env(false);
    match proxy_for(url).and_then(|proxy| ureq::Proxy::new(proxy).ok()) {
        Some(proxy) => builder.proxy(proxy),
        None => builder,
    }
}

/// A client for requests to `url`.
pub fn agent(url: &str) -> ureq::Agent {
    builder(url).build()
}

/// `ureq::get`, through the computer's trust store and proxy settings.
pub fn get(url: &str) -> ureq::Request {
    agent(url).get(url)
}

/// `ureq::post`, through the computer's trust store and proxy settings.
pub fn post(url: &str) -> ureq::Request {
    agent(url).post(url)
}

/// `ureq::request`, through the computer's trust store and proxy settings.
pub fn request(method: &str, url: &str) -> ureq::Request {
    agent(url).request(method, url)
}

/// The proxy an organisation's machine policy names (`[network] proxy` and
/// `no_proxy`), set once at start by whoever read the policy. It takes the
/// place of the environment: on a managed computer the organisation decides
/// how its traffic leaves, and a person's shell cannot route around it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicyProxy {
    /// `http://host:port`, used for http and https alike. `None` with the
    /// policy set means "connect directly", whatever the environment says.
    pub proxy: Option<String>,
    /// The `NO_PROXY` list, in the same form.
    pub no_proxy: Option<String>,
}

static POLICY_PROXY: OnceLock<PolicyProxy> = OnceLock::new();

/// Install the policy's proxy for this process. The first call wins; a
/// second call is ignored and returns `false`, so nothing later in a run can
/// swap the route the policy chose.
pub fn set_policy_proxy(proxy: PolicyProxy) -> bool {
    POLICY_PROXY.set(proxy).is_ok()
}

/// The proxy for `url`, or `None` to connect directly. In order: the
/// machine policy's when one was installed; the environment's proxy
/// variables when any is set; the operating system's own proxy settings.
pub fn proxy_for(url: &str) -> Option<String> {
    if let Some(policy) = POLICY_PROXY.get() {
        return proxy_for_policy(url, policy);
    }
    let env = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    };
    let any_set = ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY"]
        .iter()
        .any(|name| {
            env(name)
                .or_else(|| env(&name.to_ascii_lowercase()))
                .is_some()
        });
    match system_proxy() {
        Some(system) if !any_set => proxy_for_system(url, system),
        _ => proxy_for_with(url, env),
    }
}

/// [`proxy_for`] under the operating system's settings.
fn proxy_for_system(url: &str, system: &SystemProxy) -> Option<String> {
    proxy_for_with(url, |name| match name {
        "HTTPS_PROXY" => system.https.clone(),
        "HTTP_PROXY" => system.http.clone(),
        "NO_PROXY" => system.bypass.clone(),
        _ => None,
    })
}

/// [`proxy_for`] under a policy, which names one proxy for every scheme.
fn proxy_for_policy(url: &str, policy: &PolicyProxy) -> Option<String> {
    proxy_for_with(url, |name| match name {
        "ALL_PROXY" => policy.proxy.clone(),
        "NO_PROXY" => policy.no_proxy.clone(),
        _ => None,
    })
}

/// [`proxy_for`] with the environment passed in, so it can be tested.
pub fn proxy_for_with(url: &str, env: impl Fn(&str) -> Option<String>) -> Option<String> {
    let (scheme, host) = scheme_and_host(url)?;
    if is_loopback(&host) {
        return None;
    }
    let either = |upper: &str| env(upper).or_else(|| env(&upper.to_ascii_lowercase()));
    if let Some(list) = either("NO_PROXY") {
        if bypasses(&list, &host) {
            return None;
        }
    }
    let specific = match scheme.as_str() {
        "https" | "wss" => either("HTTPS_PROXY"),
        "http" | "ws" => either("HTTP_PROXY"),
        _ => None,
    };
    specific.or_else(|| either("ALL_PROXY"))
}

/// The scheme and the host (lowercase, without brackets or port) of a URL.
fn scheme_and_host(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(inner) = authority.strip_prefix('[') {
        inner.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    if host.is_empty() {
        return None;
    }
    Some((scheme.to_ascii_lowercase(), host.to_ascii_lowercase()))
}

fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "::1"
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Does a `NO_PROXY` list name this host? The common reading: entries are
/// separated by commas, `*` means every host, an entry matches the host
/// itself and every name under it (`example.com` and `.example.com` both
/// cover `api.example.com`), and a port on an entry is ignored. Address
/// ranges (`10.0.0.0/8`) are not read; name the hosts.
fn bypasses(list: &str, host: &str) -> bool {
    list.split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .any(|entry| {
            if entry == "*" {
                return true;
            }
            // Windows' and macOS's word for "a name with no dots": a host on
            // the local network reached by its short name.
            if entry.eq_ignore_ascii_case("<local>") {
                return !host.contains('.') && !host.contains(':');
            }
            let entry = entry.to_ascii_lowercase();
            let entry = entry.strip_prefix("*.").unwrap_or(&entry);
            let entry = entry.trim_start_matches('.');
            let entry = match entry.strip_prefix('[') {
                Some(inner) => inner.split(']').next().unwrap_or(""),
                None if entry.matches(':').count() == 1 => entry.split(':').next().unwrap_or(""),
                None => entry,
            };
            !entry.is_empty() && (host == entry || host.ends_with(&format!(".{entry}")))
        })
}

/// A TCP connection to `host:port` for `url`, through the proxy when one
/// applies (an HTTP `CONNECT` tunnel), for clients that do their own
/// protocol on top -- the websocket client. TLS, if any, goes on top of
/// what this returns.
pub fn tcp_for(url: &str, host: &str, port: u16, timeout: Duration) -> std::io::Result<TcpStream> {
    tcp_through(proxy_for(url), host, port, timeout)
}

/// [`tcp_for`] with the proxy already chosen.
fn tcp_through(
    proxy: Option<String>,
    host: &str,
    port: u16,
    timeout: Duration,
) -> std::io::Result<TcpStream> {
    let Some(proxy) = proxy else {
        return connect(host, port, timeout);
    };
    let proxy = ProxyAddress::parse(&proxy).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("the proxy setting `{proxy}` is not an http://host:port address"),
        )
    })?;
    let mut stream = connect(&proxy.host, proxy.port, timeout)?;
    let target = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut ask = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if let Some(credentials) = &proxy.credentials {
        ask.push_str(&format!(
            "Proxy-Authorization: Basic {}\r\n",
            base64(credentials.as_bytes())
        ));
    }
    ask.push_str("\r\n");
    stream.write_all(ask.as_bytes())?;
    stream.set_read_timeout(Some(timeout))?;
    // Read the reply one byte at a time so nothing the server sends after
    // the tunnel opens is swallowed by a buffer we then drop.
    let mut reply = Vec::new();
    let mut reader = BufReader::with_capacity(1, &stream);
    while !reply.ends_with(b"\r\n\r\n") {
        let got = reader.fill_buf()?;
        if got.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the proxy closed the connection before opening a tunnel",
            ));
        }
        reply.push(got[0]);
        reader.consume(1);
        if reply.len() > 16 * 1024 {
            return Err(std::io::Error::other("the proxy's reply was too long"));
        }
    }
    let status = String::from_utf8_lossy(&reply);
    let first = status.lines().next().unwrap_or("");
    if first.split_whitespace().nth(1) != Some("200") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("the proxy refused to connect to {target}: {first}"),
        ));
    }
    stream.set_read_timeout(None)?;
    Ok(stream)
}

fn connect(host: &str, port: u16, timeout: Duration) -> std::io::Result<TcpStream> {
    use std::net::ToSocketAddrs;
    let mut last = None;
    for address in (host, port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(stream) => return Ok(stream),
            Err(err) => last = Some(err),
        }
    }
    Err(last.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{host} has no address"),
        )
    }))
}

struct ProxyAddress {
    host: String,
    port: u16,
    credentials: Option<String>,
}

impl ProxyAddress {
    fn parse(proxy: &str) -> Option<Self> {
        let rest = match proxy.split_once("://") {
            Some(("http", rest)) => rest,
            Some(_) => return None,
            None => proxy,
        };
        let rest = rest.trim_end_matches('/');
        let (credentials, hostport) = match rest.rsplit_once('@') {
            Some((credentials, hostport)) => (Some(credentials.to_string()), hostport),
            None => (None, rest),
        };
        let (host, port) = match hostport.rsplit_once(':') {
            Some((host, port)) if !host.ends_with(']') || hostport.starts_with('[') => {
                (host, port.parse().ok()?)
            }
            _ => (hostport, 80),
        };
        let host = host.trim_start_matches('[').trim_end_matches(']');
        (!host.is_empty()).then(|| ProxyAddress {
            host: host.to_string(),
            port,
            credentials,
        })
    }
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Plain words for a connection refused because the server's certificate is
/// not trusted, or `None` when `error` is something else. The most common
/// cause on a company laptop is a network that inspects secure traffic with
/// a root the computer does not trust yet.
pub fn explain(error: &str) -> Option<String> {
    let lower = error.to_ascii_lowercase();
    let untrusted = lower.contains("invalid peer certificate")
        || lower.contains("unknownissuer")
        || lower.contains("certificate is not trusted")
        || lower.contains("certificate verify failed");
    untrusted.then(|| {
        "the secure connection was refused because the server's certificate is not trusted \
         by this computer. On a company network this usually means the network inspects \
         secure traffic: ask your IT team to install its root certificate on this computer \
         (Krate trusts what the computer trusts)"
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn each_scheme_takes_its_own_proxy_and_all_proxy_fills_in() {
        let e = env(&[
            ("HTTPS_PROXY", "http://secure:3128"),
            ("http_proxy", "http://plain:3128"),
        ]);
        assert_eq!(
            proxy_for_with("https://hub.krate.tech/x", &e).as_deref(),
            Some("http://secure:3128")
        );
        assert_eq!(
            proxy_for_with("wss://hub.krate.tech/room", &e).as_deref(),
            Some("http://secure:3128")
        );
        assert_eq!(
            proxy_for_with("http://example.com/", &e).as_deref(),
            Some("http://plain:3128")
        );
        let all = env(&[("ALL_PROXY", "http://every:8080")]);
        assert_eq!(
            proxy_for_with("https://a.example/", &all).as_deref(),
            Some("http://every:8080")
        );
        assert_eq!(proxy_for_with("https://a.example/", env(&[])), None);
    }

    #[test]
    fn no_proxy_names_hosts_and_everything_under_them() {
        let e = env(&[
            ("HTTPS_PROXY", "http://p:3128"),
            (
                "NO_PROXY",
                "internal.acme.com, .corp.example ,10.1.2.3,[fd00::1],git.acme.com:8443",
            ),
        ]);
        for direct in [
            "https://internal.acme.com/",
            "https://api.internal.acme.com/",
            "https://corp.example/",
            "https://x.corp.example/",
            "https://10.1.2.3/",
            "https://[fd00::1]:443/",
            "https://git.acme.com/",
        ] {
            assert_eq!(
                proxy_for_with(direct, &e),
                None,
                "{direct} should be direct"
            );
        }
        for proxied in [
            "https://acme.com/",
            "https://notinternal.acme.com.evil.io/",
            "https://xcorp.example/",
        ] {
            assert!(
                proxy_for_with(proxied, &e).is_some(),
                "{proxied} should use the proxy"
            );
        }
        let star = env(&[("HTTPS_PROXY", "http://p:3128"), ("no_proxy", "*")]);
        assert_eq!(proxy_for_with("https://anything.example/", &star), None);
    }

    #[test]
    fn a_policy_proxy_replaces_the_environment() {
        let policy = PolicyProxy {
            proxy: Some("http://proxy.acme.com:3128".into()),
            no_proxy: Some("internal.acme.com".into()),
        };
        assert_eq!(
            proxy_for_policy("https://hub.krate.tech/", &policy).as_deref(),
            Some("http://proxy.acme.com:3128")
        );
        assert_eq!(
            proxy_for_policy("http://example.com/", &policy).as_deref(),
            Some("http://proxy.acme.com:3128")
        );
        assert_eq!(
            proxy_for_policy("https://git.internal.acme.com/", &policy),
            None
        );
        assert_eq!(proxy_for_policy("http://127.0.0.1:9/", &policy), None);
        // A policy with no proxy means direct, not "fall back to the shell".
        assert_eq!(
            proxy_for_policy("https://hub.krate.tech/", &PolicyProxy::default()),
            None
        );
    }

    #[test]
    fn the_systems_settings_route_each_scheme_and_honour_local() {
        let system =
            from_windows_settings(Some("http=a:80;https=b:443"), Some("<local>;*.acme.com"));
        assert_eq!(
            proxy_for_system("https://hub.krate.tech/", &system).as_deref(),
            Some("http://b:443")
        );
        assert_eq!(
            proxy_for_system("http://example.com/", &system).as_deref(),
            Some("http://a:80")
        );
        assert_eq!(
            proxy_for_system("https://intranet/", &system),
            None,
            "<local> is a dotless name"
        );
        assert_eq!(proxy_for_system("https://git.acme.com/", &system), None);
    }

    #[test]
    fn loopback_is_always_direct() {
        let e = env(&[("ALL_PROXY", "http://p:3128")]);
        for url in [
            "http://127.0.0.1:8811/x",
            "http://localhost/",
            "http://[::1]:9/",
            "http://app.localhost/",
        ] {
            assert_eq!(proxy_for_with(url, &e), None, "{url}");
        }
    }

    #[test]
    fn a_url_with_credentials_or_a_port_still_finds_its_host() {
        assert_eq!(
            scheme_and_host("HTTPS://user:pw@Hub.Krate.Tech:8443/a?b#c"),
            Some(("https".into(), "hub.krate.tech".into()))
        );
        assert_eq!(scheme_and_host("not a url"), None);
    }

    #[test]
    fn proxy_addresses_parse_with_and_without_credentials() {
        let p = ProxyAddress::parse("http://me:secret@proxy.acme.com:3128/").unwrap();
        assert_eq!(
            (p.host.as_str(), p.port, p.credentials.as_deref()),
            ("proxy.acme.com", 3128, Some("me:secret"))
        );
        let p = ProxyAddress::parse("proxy.acme.com").unwrap();
        assert_eq!(
            (p.host.as_str(), p.port, p.credentials),
            ("proxy.acme.com", 80, None)
        );
        assert!(ProxyAddress::parse("socks5://p:1080").is_none());
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b"me:secret"), "bWU6c2VjcmV0");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");
    }

    #[test]
    fn an_untrusted_certificate_is_explained_and_nothing_else_is() {
        assert!(explain("https://hub.krate.tech/: Connection Failed: tls connection init failed: invalid peer certificate: UnknownIssuer").is_some());
        assert!(explain("Connection refused (os error 61)").is_none());
    }

    #[test]
    fn the_tunnel_asks_the_proxy_to_connect_and_hands_back_the_stream() {
        use std::io::Read;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut seen = Vec::new();
            let mut byte = [0u8; 1];
            while !seen.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                seen.push(byte[0]);
            }
            // The tunnel is open; what follows belongs to the caller.
            stream
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\nhello")
                .unwrap();
            String::from_utf8(seen).unwrap()
        });
        let proxy = format!("http://u:p@127.0.0.1:{port}");
        let mut stream =
            tcp_through(Some(proxy), "example.com", 443, Duration::from_secs(5)).unwrap();
        let mut after = [0u8; 5];
        stream.read_exact(&mut after).unwrap();
        assert_eq!(&after, b"hello", "bytes after the reply reach the caller");
        let seen = server.join().unwrap();
        assert!(
            seen.starts_with("CONNECT example.com:443 HTTP/1.1\r\n"),
            "{seen}"
        );
        assert!(
            seen.contains("Proxy-Authorization: Basic dTpw\r\n"),
            "{seen}"
        );
    }

    #[test]
    fn a_proxy_that_refuses_the_tunnel_is_named() {
        use std::io::Read;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 256];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n");
        });
        let err = tcp_through(
            Some(format!("http://127.0.0.1:{port}")),
            "example.com",
            443,
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(err.to_string().contains("407"), "{err}");
    }
}
