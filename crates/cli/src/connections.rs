//! Company connections at open (IC-952).
//!
//! An organisation's private app that declares `net.connection:<name>`, and
//! was granted it, gets that connection's credential from the hub when it
//! opens: once per run, for this person and these exact bytes, after the
//! access gate has let them open. The answer goes to the runtime as a
//! [`krate_runtime::connections::Connections`] and stays in this process's
//! memory for the run; the app never sees it, and nothing here prints or
//! writes it. What is kept on disk is the connection's address and allow
//! (never its credential), so that offline, or once the hub has refused, a
//! request to the address is refused in words rather than sent.
//!
//! A refusal does not stop the app opening: the person may still use what
//! does not need the connection. The app's request to the address is refused
//! with the hub's words, and the run says once, at the start, which
//! connection is not available and why.

use krate_manifest::Manifest;
use krate_policy::SessionPolicy;
use krate_runtime::connections::{
    open_credential, Connection, ConnectionState, Connections, Delivered,
};
use serde_json::Value;

use crate::access;

/// What the hub said about one connection.
enum Answer {
    Ready(Value),
    /// A refusal: the hub's words, and the address when it gave one.
    Refused {
        words: String,
        base: Option<String>,
    },
    /// No answer that means anything: offline, or the hub is broken.
    Unreachable,
}

fn agent(url: &str) -> ureq::Agent {
    krate_http::builder(url)
        .timeout_connect(std::time::Duration::from_secs(5))
        .timeout_read(std::time::Duration::from_secs(15))
        .build()
}

fn ask(session: &access::OrgSession, name: &str, digest: &str) -> Answer {
    let url = format!(
        "{}/orgs/{}/connections/{name}/credential/{digest}",
        session.hub, session.slug
    );
    let mut request = agent(&url).get(&url).set("Accept", "application/json");
    if let Some(authorization) = &session.authorization {
        request = request.set("Authorization", authorization);
    }
    match request.call() {
        Ok(response) => match response
            .into_string()
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        {
            Some(answer)
                // The credential itself is read by `delivered` alone.
                if answer["base"].is_string() =>
            {
                Answer::Ready(answer)
            }
            _ => Answer::Unreachable,
        },
        Err(ureq::Error::Status(status, response)) if (400..500).contains(&status) => {
            let answer: Value = response
                .into_string()
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default();
            if answer["error"].as_str() == Some("rate-limited") {
                return Answer::Unreachable;
            }
            let message = answer["message"]
                .as_str()
                .filter(|m| !m.is_empty())
                .unwrap_or("the organisation's hub refused it");
            Answer::Refused {
                words: format!("the {name} connection is not available to this app: {message}"),
                base: answer["base"].as_str().map(str::to_string),
            }
        }
        Err(_) => Answer::Unreachable,
    }
}

/// The credential part of the hub's answer, as the runtime's seam takes it.
/// Under the organisation keyring (F-316) this reads the sealed blob instead.
fn delivered(answer: &Value) -> Delivered {
    Delivered::Header {
        name: answer["header"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        value: answer["header"]["value"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The connections declared, granted, and asked for by name.
fn asked_for(manifest: &Manifest, policy: &SessionPolicy) -> Vec<String> {
    manifest
        .declared_capabilities()
        .unwrap_or_default()
        .into_iter()
        .filter(|cap| cap.module() == "net" && cap.action() == "connection")
        .filter(|cap| policy.allows(cap))
        .filter_map(|cap| cap.resource().map(str::to_string))
        .collect()
}

/// Resolve every connection this run may use. `private` answers, only when
/// asked, the organisation whose signed mark the bundle carries and the
/// bundle's digest: `None` for any app that is not an organisation's
/// private app, which never gets a connection. The access gate has already
/// let these bytes open.
pub fn resolve(
    manifest: Option<&Manifest>,
    policy: &SessionPolicy,
    private: impl FnOnce() -> Option<(String, String)>,
) -> Connections {
    let Some(manifest) = manifest else {
        return Connections::default();
    };
    let names = asked_for(manifest, policy);
    if names.is_empty() {
        return Connections::default();
    }
    let session = private().and_then(|(slug, digest)| {
        access::org_session(&digest)
            .filter(|s| s.slug == slug)
            .map(|session| (digest, session))
    });
    let Some((digest, session)) = session else {
        for name in &names {
            eprintln!(
                "this app asks for the company connection {name}, which only an \
                 organisation's private app can use, so it opens without it"
            );
        }
        return Connections::default();
    };
    let mut out = Vec::new();
    for name in names {
        let known = access::known_connection(&digest, &name);
        let known_base = known
            .as_ref()
            .and_then(|k| k["base"].as_str().map(str::to_string));
        let known_allow = |k: &Option<Value>| {
            k.as_ref()
                .map(|k| {
                    (
                        strings(&k["allow"]["methods"]),
                        strings(&k["allow"]["paths"]),
                    )
                })
                .unwrap_or_default()
        };
        match ask(&session, &name, &digest) {
            Answer::Ready(answer) => {
                let base = answer["base"].as_str().unwrap_or_default().to_string();
                access::remember_connection(&digest, &name, &base, &answer["allow"]);
                out.push(Connection {
                    name: name.clone(),
                    base,
                    methods: strings(&answer["allow"]["methods"]),
                    paths: strings(&answer["allow"]["paths"]),
                    // Opened in exactly one place, the runtime's side of
                    // the credential seam (F-316).
                    state: open_credential(delivered(&answer)).unwrap_or_else(|why| {
                        let words = format!("the {name} connection could not be used: {why}");
                        eprintln!("{words}");
                        ConnectionState::Unavailable(words)
                    }),
                });
            }
            Answer::Refused { words, base } => {
                eprintln!("{words}");
                if let Some(base) = base.or(known_base) {
                    let (methods, paths) = known_allow(&known);
                    out.push(Connection {
                        name: name.clone(),
                        base,
                        // Refused whatever the request: an unavailable
                        // connection refuses everything under its address.
                        methods,
                        paths,
                        state: ConnectionState::Unavailable(words),
                    });
                }
            }
            Answer::Unreachable => {
                let words = format!(
                    "the {name} connection needs {} to hand over its credential, and it could \
                     not be reached, so nothing is sent with it: connect to the internet and \
                     open the app again",
                    session.hub
                );
                eprintln!("{words}");
                if let Some(base) = known_base {
                    let (methods, paths) = known_allow(&known);
                    out.push(Connection {
                        name: name.clone(),
                        base,
                        methods,
                        paths,
                        state: ConnectionState::Unavailable(words),
                    });
                }
            }
        }
    }
    Connections::new(out)
}
