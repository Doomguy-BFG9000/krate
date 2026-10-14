//! Company connections (IC-952): an organisation's API, reached with a
//! credential the app never holds.
//!
//! An organisation's admin sets a connection up on the hub once: a name, the
//! base it covers (scheme, host, port and a path prefix), what an app may do
//! there (methods and path prefixes), and a credential (one header). When a
//! private app that declares `net.connection:<name>` opens, the CLI asks the
//! hub for that connection for this person and this release and hands the
//! answer to the run as a [`Connections`]. Nothing of it reaches the app.
//!
//! Every request the app makes passes through [`Connections::route`] before
//! the ordinary `net.connect` check:
//!
//! - a request outside every connection's base is an ordinary request, and is
//!   judged by `net.connect` as before -- no credential is ever added to it;
//! - a request inside a base is the connection's: its method and path must be
//!   ones the connection allows, the credential must be in hand (the hub
//!   answered this run), and only then is the header added, replacing any
//!   header of the same name the app set. Anything else is refused with words
//!   the app can show.
//!
//! The credential lives in this process's memory for this run only. It is
//! never written anywhere, never in a `Debug` print, and the app's own
//! memory never holds it: the header is added here, on the host side of the
//! boundary, after the app's request has left the app. Redirects are never
//! followed by the transport (`fetch_over_tls` sets `redirects(0)`, and the
//! plain path has no redirect code), so a redirect off the base cannot carry
//! it; a new request the app makes to the redirect's target is routed again,
//! and gets the credential only if that target is inside the base too.

use krate_adapter_common::net::PlainHttpUrl;

/// A credential value: shown as hidden in every `Debug` print.
#[derive(Clone, PartialEq, Eq)]
pub struct Credential(String);

impl Credential {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    fn reveal(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credential(<hidden>)")
    }
}

/// What the hub handed over for one connection, before it is a header: the
/// device's side of the credential seam (F-316).
///
/// The credential was sealed where it was typed (the admin's browser or
/// CLI) to the organisation's public key, for a context binding the
/// organisation, the connection's name, its header and its address. The hub
/// keeps and hands over that blob; it cannot open it. Only
/// [`open_credential`] does, with the organisation key this device holds.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Delivered {
    /// The organisation's short name.
    pub org: String,
    pub name: String,
    /// The connection's address, as the hub keeps it.
    pub base: String,
    pub header_name: String,
    /// The version of the organisation key it is sealed to.
    pub version: u32,
    /// Base64 of the HPKE seal. Ciphertext: printing it shows nothing.
    pub sealed: String,
}

/// The longest header value the runtime sends (adapter-common net.rs).
const MAX_VALUE: usize = 4096;

/// Turn what the hub handed over into the header this run adds, or the
/// words of why it cannot be used. The one place on the device a credential
/// is opened. `keyring` is this device's copy of the organisation's key, or
/// the words of why it has none.
pub fn open_credential(
    delivered: Delivered,
    keyring: Result<&krate_orgkeys::Keyring, String>,
) -> Result<ConnectionState, String> {
    let org = &delivered.org;
    let keyring = keyring?;
    if keyring.version(delivered.version).is_none() {
        return Err(format!(
            "this computer does not have {org}'s newest encryption key yet, which the credential \
             needs: it gets it the next time another member's computer is online with one of \
             {org}'s apps open, then open the app again"
        ));
    }
    let context = krate_orgkeys::connection_context(
        org,
        &delivered.name,
        &delivered.header_name,
        &delivered.base,
    );
    let opened =
        krate_orgkeys::open_for_org(keyring, delivered.version, &context, &delivered.sealed)
            .map_err(|_| {
                format!(
                "its credential does not open on this computer: it was changed on the way, or was \
                 entered for another address. An owner or admin of {org} enters it again with \
                 Replace credential"
            )
            })?;
    let value = String::from_utf8(opened).map_err(|_| {
        format!("its credential is not text: an owner or admin of {org} enters it again")
    })?;
    if delivered.header_name.is_empty()
        || value.trim().is_empty()
        || value.len() > MAX_VALUE
        || value.contains(['\r', '\n', '\0'])
    {
        return Err(format!(
            "its credential is not one line of up to {MAX_VALUE} characters: an owner or admin of \
             {org} enters it again"
        ));
    }
    Ok(ConnectionState::Ready {
        header_name: delivered.header_name,
        value: Credential::new(value),
    })
}

/// Where a connection stands for this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// The hub handed over the credential: this header goes on each allowed
    /// request to the base.
    Ready {
        header_name: String,
        value: Credential,
    },
    /// No credential this run, and why, in words the app can show: the hub
    /// refused (removed connection, outside its audience, app not allowed),
    /// or could not be reached. A request to the base is refused with them.
    Unavailable(String),
}

/// One connection this run knows about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub name: String,
    /// `https://host[:port]/path`, as the hub keeps it.
    pub base: String,
    /// The methods it allows, upper-case.
    pub methods: Vec<String>,
    /// The path prefixes under the base it allows, each starting with `/`.
    pub paths: Vec<String>,
    pub state: ConnectionState,
}

/// A connection's base, parsed once.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Base {
    tls: bool,
    host: String,
    port: u16,
    /// The path prefix without a trailing slash; "" for the root.
    prefix: String,
}

impl Base {
    fn parse(text: &str) -> Option<Base> {
        let url = PlainHttpUrl::parse(text).ok()?;
        if url.path_and_query.contains('?') {
            return None;
        }
        let prefix = url.path_and_query.trim_end_matches('/').to_string();
        Some(Base {
            tls: url.tls,
            host: url.host,
            port: url.port,
            prefix,
        })
    }

    /// The part of `path` under this base ("/" for the base itself), or
    /// `None` when it is not under it. Whole segments only: a base of
    /// `/finance` does not cover `/finance-old`.
    fn under<'p>(&self, path: &'p str) -> Option<&'p str> {
        if self.prefix.is_empty() {
            return Some(path);
        }
        let rest = path.strip_prefix(self.prefix.as_str())?;
        if rest.is_empty() {
            Some("/")
        } else if rest.starts_with('/') {
            Some(rest)
        } else {
            None
        }
    }
}

/// What to do with one request.
#[derive(Debug, PartialEq, Eq)]
pub enum Route<'a> {
    /// Not inside any connection's base: an ordinary request.
    Plain,
    /// Inside a base, and not sent: the words say why.
    Refused(String),
    /// Inside a base and allowed: add this header. `host` and `port` are
    /// where it goes, for the organisation's network ceiling.
    Credential {
        name: &'a str,
        header_name: &'a str,
        value: &'a str,
        host: String,
        port: u16,
    },
}

/// The company connections one run may use.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Connections {
    entries: Vec<(Connection, Base)>,
}

/// Does this path try to step outside what a prefix check sees? A dot
/// segment (also behind a `;` path parameter), a backslash, or a
/// percent-escaped dot, slash, backslash or semicolon could be read by the
/// server as a different path from the one checked here.
fn ambiguous(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    path.contains('\\')
        || lower.contains("%2e")
        || lower.contains("%2f")
        || lower.contains("%5c")
        || lower.contains("%3b")
        || path.split('/').any(|segment| {
            // `..;x` is `..` to a server that drops path parameters (Tomcat,
            // Spring, some proxies): the classic way past a prefix check.
            let bare = segment.split(';').next().unwrap_or(segment);
            bare == "." || bare == ".."
        })
}

fn loopback(host: &str) -> bool {
    host == "127.0.0.1" || host == "localhost" || host == "::1" || host == "[::1]"
}

/// "GET", "GET or POST", "GET, POST or PUT".
fn spoken(items: &[String]) -> String {
    match items {
        [] => "nothing".to_string(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

impl Connections {
    /// The connections the hub gave this run. One whose base does not parse
    /// is dropped: it could never match, and guessing at it could.
    pub fn new(connections: Vec<Connection>) -> Self {
        let entries = connections
            .into_iter()
            .filter_map(|c| Base::parse(&c.base).map(|base| (c, base)))
            .collect();
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(c, _)| c.name.as_str())
    }

    /// Route one request. `granted` says whether this run holds
    /// `net.connection:<name>`: a connection the run was not granted is not
    /// its own, and a request to its base is an ordinary one.
    pub fn route(&self, method: &str, url: &str, granted: impl Fn(&str) -> bool) -> Route<'_> {
        let Ok(target) = PlainHttpUrl::parse(url) else {
            return Route::Plain;
        };
        let path = target
            .path_and_query
            .split('?')
            .next()
            .unwrap_or("/")
            .to_string();
        // The longest base that covers the request is the one it is for.
        let Some((connection, _, under)) = self
            .entries
            .iter()
            .filter(|(c, base)| {
                base.tls == target.tls
                    && base.host == target.host
                    && base.port == target.port
                    && granted(&c.name)
            })
            .filter_map(|(c, base)| base.under(&path).map(|under| (c, base, under)))
            .max_by_key(|(_, base, _)| base.prefix.len())
        else {
            return Route::Plain;
        };
        let name = &connection.name;
        if ambiguous(&path) {
            return Route::Refused(format!(
                "the {name} connection did not send this request: its address has a part \
                 (., .., or an escaped / or .) that could reach somewhere the connection \
                 does not allow. Use the plain path"
            ));
        }
        // No credential this run: everything under the address is refused
        // with the reason, whatever its method or path.
        let (header_name, value) = match &connection.state {
            ConnectionState::Unavailable(words) => return Route::Refused(words.clone()),
            ConnectionState::Ready { header_name, value } => (header_name, value),
        };
        let method = method.to_ascii_uppercase();
        if !connection.methods.iter().any(|m| m == &method) {
            return Route::Refused(format!(
                "the {name} connection lets this app use {} only, so this {method} was not \
                 sent: ask an admin of your organisation to allow {method} on {name}",
                spoken(&connection.methods)
            ));
        }
        let allowed_path = connection.paths.iter().any(|prefix| {
            let prefix = prefix.trim_end_matches('/');
            prefix.is_empty()
                || under == prefix
                || under
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('/'))
        });
        if !allowed_path {
            return Route::Refused(format!(
                "the {name} connection does not reach {under} under {}, so this request was \
                 not sent: it allows {}. Ask an admin of your organisation to add the path",
                connection.base,
                spoken(&connection.paths)
            ));
        }
        if !target.tls && !loopback(&target.host) {
            return Route::Refused(format!(
                "the {name} connection sends its credential only over https, so this request \
                 to {} was not sent",
                target.host
            ));
        }
        Route::Credential {
            name,
            header_name,
            value: value.reveal(),
            host: target.host,
            port: target.port,
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_dot_segment_hidden_behind_a_path_parameter_is_refused() {
        for path in [
            "/invoices/..;/admin",
            "/invoices/.;/x",
            "/..;a/admin",
            "/invoices/..%3B/admin",
        ] {
            assert!(ambiguous(path), "{path} must not pass as a plain path");
        }
        // An ordinary parameter on an ordinary segment is fine.
        assert!(!ambiguous("/invoices;page=2"));
        assert!(!ambiguous("/rates/eur"));
    }
    use super::*;

    const SECRET: &str = "Bearer sk-test-only-0123456789";

    fn finance(state: ConnectionState) -> Connection {
        Connection {
            name: "finance-api".into(),
            base: "https://api.acme.example/finance".into(),
            methods: vec!["GET".into(), "POST".into()],
            paths: vec!["/invoices".into(), "/rates/".into()],
            state,
        }
    }

    fn ready() -> ConnectionState {
        ConnectionState::Ready {
            header_name: "Authorization".into(),
            value: Credential::new(SECRET),
        }
    }

    fn yes(_: &str) -> bool {
        true
    }

    #[test]
    fn a_request_inside_the_base_and_its_allow_gets_the_header() {
        let c = Connections::new(vec![finance(ready())]);
        for url in [
            "https://api.acme.example/finance/invoices",
            "https://API.acme.example:443/finance/invoices/2026?page=2",
            "https://api.acme.example/finance/rates/eur",
        ] {
            match c.route("get", url, yes) {
                Route::Credential {
                    name,
                    header_name,
                    value,
                    host,
                    port,
                } => {
                    assert_eq!(
                        (name, header_name, value),
                        ("finance-api", "Authorization", SECRET)
                    );
                    assert_eq!((host.as_str(), port), ("api.acme.example", 443));
                }
                other => panic!("{url}: {other:?}"),
            }
        }
    }

    #[test]
    fn outside_the_base_is_an_ordinary_request() {
        let c = Connections::new(vec![finance(ready())]);
        for url in [
            "https://api.acme.example/other",
            "https://api.acme.example/finance-old/invoices",
            "https://api.acme.example:8443/finance/invoices",
            "http://api.acme.example/finance/invoices",
            "https://evil.example/finance/invoices",
            "https://api.acme.example.evil.example/finance/invoices",
        ] {
            assert_eq!(c.route("GET", url, yes), Route::Plain, "{url}");
        }
    }

    #[test]
    fn a_wrong_method_or_path_is_refused_in_words() {
        let c = Connections::new(vec![finance(ready())]);
        let Route::Refused(words) =
            c.route("DELETE", "https://api.acme.example/finance/invoices/1", yes)
        else {
            panic!("a DELETE is not allowed")
        };
        assert!(words.contains("GET or POST only"), "{words}");
        assert!(words.contains("allow DELETE"), "{words}");
        for path in ["/finance", "/finance/payroll", "/finance/invoicesx"] {
            let Route::Refused(words) =
                c.route("GET", &format!("https://api.acme.example{path}"), yes)
            else {
                panic!("{path} is outside the allowed paths")
            };
            assert!(words.contains("does not reach"), "{words}");
        }
    }

    #[test]
    fn a_path_that_could_step_outside_is_refused() {
        let c = Connections::new(vec![finance(ready())]);
        for path in [
            "/finance/invoices/../payroll",
            "/finance/invoices/%2e%2e/payroll",
            "/finance/invoices%2Fpayroll",
            "/finance/invoices/./x",
            "/finance/invoices\\..\\payroll",
        ] {
            let url = format!("https://api.acme.example{path}");
            assert!(
                matches!(c.route("GET", &url, yes), Route::Refused(_) | Route::Plain),
                "{path}"
            );
            assert!(
                !matches!(c.route("GET", &url, yes), Route::Credential { .. }),
                "{path} must never carry the credential"
            );
        }
    }

    #[test]
    fn no_credential_this_run_refuses_with_the_hubs_words() {
        let words = "the finance-api connection was removed".to_string();
        let c = Connections::new(vec![finance(ConnectionState::Unavailable(words.clone()))]);
        assert_eq!(
            c.route("GET", "https://api.acme.example/finance/invoices", yes),
            Route::Refused(words)
        );
    }

    #[test]
    fn a_connection_the_run_was_not_granted_is_not_its_own() {
        let c = Connections::new(vec![finance(ready())]);
        assert_eq!(
            c.route("GET", "https://api.acme.example/finance/invoices", |_| {
                false
            }),
            Route::Plain
        );
    }

    #[test]
    fn plain_http_carries_it_only_to_this_machine() {
        let mut lan = finance(ready());
        lan.base = "http://10.0.0.5/finance".into();
        let mut local = finance(ready());
        local.name = "local".into();
        local.base = "http://127.0.0.1:9000/finance".into();
        let c = Connections::new(vec![lan, local]);
        assert!(matches!(
            c.route("GET", "http://10.0.0.5/finance/invoices", yes),
            Route::Refused(_)
        ));
        assert!(matches!(
            c.route("GET", "http://127.0.0.1:9000/finance/invoices", yes),
            Route::Credential { name: "local", .. }
        ));
    }

    #[test]
    fn the_longest_base_wins_and_the_root_covers_everything() {
        let mut root = finance(ready());
        root.name = "whole".into();
        root.base = "https://api.acme.example".into();
        root.paths = vec!["/".into()];
        root.methods = vec!["GET".into()];
        let c = Connections::new(vec![root, finance(ready())]);
        assert!(matches!(
            c.route("GET", "https://api.acme.example/finance/invoices", yes),
            Route::Credential {
                name: "finance-api",
                ..
            }
        ));
        assert!(matches!(
            c.route("GET", "https://api.acme.example/anything", yes),
            Route::Credential { name: "whole", .. }
        ));
    }

    /// What the hub hands over: `value` sealed to `ring`'s current key for
    /// finance-api at its address.
    fn delivered(ring: &krate_orgkeys::Keyring, value: &str) -> Delivered {
        let (version, key) = ring.current();
        let base = "https://api.acme.example/finance";
        let context =
            krate_orgkeys::connection_context("acme", "finance-api", "Authorization", base);
        Delivered {
            org: "acme".into(),
            name: "finance-api".into(),
            base: base.into(),
            header_name: "Authorization".into(),
            version,
            sealed: krate_orgkeys::seal_for_org(&key.public(), &context, value.as_bytes()).unwrap(),
        }
    }

    #[test]
    fn the_seam_opens_the_sealed_credential_with_this_devices_key() {
        let ring = krate_orgkeys::Keyring::create("acme");
        let d = delivered(&ring, SECRET);
        assert!(!format!("{d:?}").contains("sk-test-only"), "{d:?}");
        assert_eq!(open_credential(d.clone(), Ok(&ring)).unwrap(), ready());
        // Another organisation key, as a removed member keeps: refused.
        let other = krate_orgkeys::Keyring::create("acme");
        let words = open_credential(d.clone(), Ok(&other)).unwrap_err();
        assert!(words.contains("does not open on this computer"), "{words}");
        // A hub that moved it to another address hands over a blob that no
        // longer opens.
        let mut moved = d.clone();
        moved.base = "https://evil.example/collect".into();
        assert!(open_credential(moved, Ok(&ring)).is_err());
        // Sealed to a version this device does not hold yet.
        let mut newer = d.clone();
        newer.version = 2;
        let words = open_credential(newer, Ok(&ring)).unwrap_err();
        assert!(words.contains("newest encryption key yet"), "{words}");
        // No key on this computer: the caller's words, unchanged.
        assert_eq!(
            open_credential(d, Err("waiting".into())),
            Err("waiting".to_string())
        );
        // Opened, but not a header value the runtime could send.
        for bad in ["", "Bearer a\r\nX-Evil: 1", &"x".repeat(4097)] {
            assert!(
                open_credential(delivered(&ring, bad), Ok(&ring)).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_credential_never_shows_in_a_debug_print() {
        let c = Connections::new(vec![finance(ready())]);
        let printed = format!("{c:?}");
        assert!(!printed.contains("sk-test-only"), "{printed}");
        assert!(printed.contains("<hidden>"), "{printed}");
    }
}
