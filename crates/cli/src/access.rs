//! Opening an organisation's private app, online or off (IC-948 G and H).
//!
//! Every door that runs an app -- `krate run`, `krate launch`, a double-click
//! through the Player, Studio's Open, the MCP tools -- reaches [`gate`] once
//! the bytes are in hand. For a signed bundle it asks the hub whether these
//! exact bytes are an organisation's private release:
//!
//! - **Online**, the hub's answer is the check-in. A member opens it, and the
//!   offline lease is fetched, or refreshed once it is past half its life. A
//!   refused check-in (removed, never shared) deletes the lease and nothing
//!   runs; the person's app data is not touched.
//! - **Offline**, a private app opens only on a stored lease for this digest
//!   and this sign-in that verifies under the pinned hub key and has not run
//!   out ([`krate_lease::check`]).
//!
//! Before the first private open the person is shown the offline terms with
//! the exact date, and again whenever a shorter class replaces a longer one.
//!
//! Leases live in `~/.krate/leases/<digest>.json`. Which bytes the hub said
//! were public is remembered beside them, so an ordinary signed app asks
//! once and never again. Unsigned bundles are never asked about: an
//! organisation only ever releases signed ones.
//!
//! The honest limit, said in the docs too: this is enforced by Krate on the
//! device. Somebody who copies the bytes and rebuilds them is no longer
//! opening the organisation's signed app, but Krate cannot stop a copy.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use crate::{github_auth, org};

/// How a door is being used: from a terminal, or with nobody at one (a
/// double-click), where the words also go in a native alert.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Door {
    Terminal,
    Window,
}

/// Where the bytes being opened came from.
pub enum Bytes<'a> {
    /// A file on this machine; hashed only if it is signed.
    File(&'a Path),
    /// A download whose sha256 is already known, from this link.
    Fetched { sha256: &'a str, link: &'a str },
}

fn leases_dir() -> PathBuf {
    crate::krate_home().join("leases")
}

fn record_path(digest: &str) -> PathBuf {
    leases_dir().join(format!("{digest}.json"))
}

fn public_path() -> PathBuf {
    leases_dir().join("known.json")
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// What is kept about one private app on this machine.
fn load_record(digest: &str) -> Option<Value> {
    let text = std::fs::read_to_string(record_path(digest)).ok()?;
    serde_json::from_str(&text).ok()
}

fn save_record(digest: &str, record: &Value) {
    let path = record_path(digest);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let staged = path.with_extension(format!("json.{}", std::process::id()));
    if std::fs::write(&staged, record.to_string()).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600));
        }
        if std::fs::rename(&staged, &path).is_err() {
            let _ = std::fs::remove_file(&staged);
        }
    }
}

/// A refused check-in: the lease goes, and with it offline access. Only the
/// lease -- the app's own data is the person's and stays (R10). What is kept
/// is that these bytes are private, so the next open checks in again rather
/// than opening as an ordinary app.
fn forget(digest: &str) {
    if let Some(mut record) = load_record(digest) {
        record["answer"] = Value::Null;
        record["class"] = serde_json::json!("online");
        record["refused_at"] = serde_json::json!(now());
        save_record(digest, &record);
    }
}

/// What the hub said about signed bytes that are not an organisation's:
/// `public` for good, `unknown` (never published) for a day, after which
/// it is asked again -- bytes the hub has never seen may become a
/// candidate later.
fn already_answered(digest: &str) -> bool {
    let known: Value = std::fs::read_to_string(public_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let entry = &known[digest];
    match entry["state"].as_str() {
        Some("public") => true,
        Some("unknown") => entry["at"].as_u64().unwrap_or(0) + 86_400 > now(),
        _ => false,
    }
}

fn remember(digest: &str, state: &str) {
    let mut known: Value = std::fs::read_to_string(public_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    known[digest] = serde_json::json!({ "state": state, "at": now() });
    // Bounded: the newest thousand are plenty to stop repeat questions.
    if let Some(map) = known.as_object_mut() {
        while map.len() > 1000 {
            let oldest = map
                .iter()
                .min_by_key(|(_, v)| v["at"].as_u64().unwrap_or(0))
                .map(|(k, _)| k.clone());
            match oldest {
                Some(key) => {
                    map.remove(&key);
                }
                None => break,
            }
        }
    }
    let _ = std::fs::create_dir_all(leases_dir());
    let _ = std::fs::write(public_path(), known.to_string());
}

/// The hub's view of these bytes for this person.
enum Look {
    /// Not an organisation's: open as any app.
    Public,
    /// The hub has never heard of these bytes.
    Unknown,
    /// An organisation's private app: the hub's answer for this person.
    Private(std::result::Result<Value, krate_bundle::BundleError>),
    /// The hub could not be reached.
    Offline,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(5))
        .timeout_read(std::time::Duration::from_secs(15))
        .build()
}

/// Ask the hub about `digest`. A HEAD first, which downloads nothing and is
/// not counted as a download, then for a private app the small JSON answer
/// (Addendum 2, item 6) that names the organisation or the reason.
fn look(hub: &str, digest: &str, authorization: Option<&str>) -> Look {
    let url = format!("{hub}/a/{digest}");
    let with_auth = |request: ureq::Request| match authorization {
        Some(value) => request.set("Authorization", value),
        None => request,
    };
    let head = with_auth(agent().head(&url).set("Accept", "application/json")).call();
    match head {
        Ok(response) => {
            if !response.content_type().contains("json") {
                return Look::Public;
            }
        }
        Err(ureq::Error::Status(401 | 403, _)) => {}
        // Never heard of: an ordinary app. Anything else (busy, rate
        // limited, broken) says nothing about these bytes, so it is treated
        // as no answer at all -- never as permission.
        Err(ureq::Error::Status(404, _)) => return Look::Unknown,
        Err(ureq::Error::Status(_, _)) => return Look::Offline,
        Err(ureq::Error::Transport(_)) => return Look::Offline,
    }
    let answer = with_auth(agent().get(&url).set("Accept", "application/json")).call();
    match answer {
        Ok(response) => match response.into_string() {
            Ok(text) => Look::Private(Ok(serde_json::from_str(&text).unwrap_or_default())),
            Err(_) => Look::Offline,
        },
        Err(ureq::Error::Status(status, response)) => {
            let body = response.into_string().unwrap_or_default();
            let answer: Value = serde_json::from_str(&body).unwrap_or_default();
            Look::Private(Err(krate_bundle::BundleError::HubRefused {
                url: url.clone(),
                status,
                reason: answer["error"]
                    .as_str()
                    .unwrap_or("not-a-member")
                    .to_string(),
                message: answer["message"].as_str().unwrap_or("").to_string(),
                org: answer["org"].as_str().map(str::to_string),
            }))
        }
        Err(ureq::Error::Transport(_)) => Look::Offline,
    }
}

/// `GET /orgs/<slug>/leases/<digest>`: the hub's lease answer, `None` when
/// the hub has none to give (offline, no such route yet).
fn fetch_lease(
    hub: &str,
    slug: &str,
    digest: &str,
    authorization: &str,
) -> std::result::Result<Option<Value>, String> {
    let url = format!("{hub}/orgs/{slug}/leases/{digest}");
    match agent()
        .get(&url)
        .set("Authorization", authorization)
        .set("Accept", "application/json")
        .call()
    {
        Ok(response) => Ok(response
            .into_string()
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())),
        Err(ureq::Error::Status(_, response)) => {
            let answer: Value = serde_json::from_str(&response.into_string().unwrap_or_default())
                .unwrap_or_default();
            match answer["error"].as_str() {
                Some(reason @ ("removed" | "not-a-member")) => Err(reason.to_string()),
                _ => Ok(None),
            }
        }
        Err(ureq::Error::Transport(_)) => Ok(None),
    }
}

/// The date a lease runs out, as a person reads it.
fn date(secs: u64) -> String {
    org::when(&serde_json::json!(secs))
}

/// Show `words` where the person is looking: always on stderr, and in a
/// native alert when nobody is at a terminal to read it.
pub fn tell(door: Door, title: &str, words: &str) {
    eprintln!("{words}");
    alert(door, title, words);
}

/// The native alert alone, for a window door nobody is watching from a
/// terminal.
fn alert(door: Door, title: &str, words: &str) {
    alert_where(door, title, words, true);
}

/// `on_windows` is false for a refusal: on Windows `main` already puts any
/// refusal in a dialog when nobody can read stderr, and two would be one
/// too many.
fn alert_where(door: Door, title: &str, words: &str, on_windows: bool) {
    if door != Door::Window {
        return;
    }
    #[cfg(target_os = "macos")]
    if !stderr_is_watched() {
        krate_adapter_macos::present_message(title, words);
    }
    #[cfg(windows)]
    if on_windows {
        // The same test as `main`'s dialog: no console and nobody reading
        // stderr is a double-click (K-178, K-240).
        let unwatched =
            unsafe { windows_sys::Win32::System::Console::GetConsoleWindow().is_null() }
                && !crate::stderr_has_a_reader();
        if unwatched {
            let _ = rfd::MessageDialog::new()
                .set_title(title)
                .set_description(words)
                .set_buttons(rfd::MessageButtons::Ok)
                .show();
        }
    }
    let _ = (title, words, on_windows);
}

/// Is somebody reading stderr -- a terminal, a pipe, a file? Then a modal
/// alert would only block a process nobody can see (K-240). A double-click
/// sends stderr to /dev/null, which is none of them.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn stderr_is_watched() -> bool {
    use std::io::IsTerminal;
    if std::io::stderr().is_terminal() {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(std::io::stderr().as_raw_fd(), &mut stat) } == 0 {
            let kind = stat.st_mode & libc::S_IFMT;
            return kind == libc::S_IFIFO || kind == libc::S_IFREG || kind == libc::S_IFSOCK;
        }
        true
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The offline terms, shown before the first private open (Addendum 3 G).
fn notice(org_name: &str, class: &str, expires_at: Option<u64>) -> String {
    match expires_at {
        Some(at) if class != "online" => format!(
            "This is {org_name}'s private app. Works offline until {}, then needs a \
             connection. If you are removed, it stops when it next connects.",
            date(at)
        ),
        _ => format!(
            "This is {org_name}'s private app. It needs a connection every time it opens. If \
             you are removed, it stops at once."
        ),
    }
}

/// May these bytes open? `Ok` for anything that is not an organisation's
/// private app, and for a private one this person may open now.
///
/// `authority` is the root the bundle's signature chains to, `None` for an
/// unsigned one. Only signed bytes are asked about -- an organisation never
/// releases unsigned ones -- and each public one only once.
pub fn gate(bytes: Bytes, authority: Option<&str>, door: Door) -> Result<()> {
    if authority.is_none() {
        return Ok(());
    }
    let (digest, link) = match bytes {
        Bytes::Fetched { sha256, link } => (sha256.to_string(), Some(link)),
        Bytes::File(path) => match std::fs::read(path) {
            Ok(bytes) => (krate_bundle::sha256_hex(&bytes), None),
            Err(_) => return Ok(()),
        },
    };
    let record = load_record(&digest);
    if record.is_none() && already_answered(&digest) {
        return Ok(());
    }
    let identity = github_auth::current();
    // The hub to ask: the one that served the link, the one this app came
    // from before, or the one this machine uses.
    let hub = link
        .filter(|l| org::is_hub_link(l))
        .and_then(org::origin)
        .or_else(|| {
            record
                .as_ref()
                .and_then(|r| r["hub"].as_str().map(str::to_string))
        })
        .unwrap_or_else(|| org::hub_url(None));
    // The session goes only to a Krate hub, as on every other path.
    let authorization = identity
        .as_ref()
        .filter(|_| org::is_hub_link(&format!("{hub}/")))
        .map(|identity| format!("Bearer {}", identity.token));
    let me = identity
        .as_ref()
        .map(|identity| org::principal_of(&identity.login));

    match look(&hub, &digest, authorization.as_deref()) {
        Look::Public => {
            if record.is_some() {
                let _ = std::fs::remove_file(record_path(&digest));
            }
            remember(&digest, "public");
            Ok(())
        }
        Look::Unknown => match record {
            // The hub no longer knows bytes it once released.
            Some(record) => {
                forget(&digest);
                let org_name = record["org"]["name"].as_str().unwrap_or("its organisation");
                refuse(
                    door,
                    &format!(
                        "this was {org_name}'s private app, and the hub no longer has it, so \
                         it does not open: ask {org_name} for its current link"
                    ),
                )
            }
            None => {
                remember(&digest, "unknown");
                Ok(())
            }
        },
        Look::Offline => match record {
            Some(record) => offline(&digest, &record, me.as_deref(), door),
            // Never seen online: there is nothing to say it is private.
            None => Ok(()),
        },
        Look::Private(Err(krate_bundle::BundleError::HubRefused {
            reason,
            message,
            org,
            ..
        })) => {
            if matches!(
                reason.as_str(),
                "removed" | "not-a-member" | "digest-mismatch" | "no-such-org"
            ) {
                if record.is_none() {
                    save_record(
                        &digest,
                        &serde_json::json!({
                            "digest": digest, "hub": hub, "org": { "slug": org, "name": org },
                            "class": "online", "answer": null,
                        }),
                    );
                }
                forget(&digest);
            }
            let org = org.or_else(|| {
                record
                    .as_ref()
                    .and_then(|r| r["org"]["slug"].as_str().map(str::to_string))
            });
            let words = org::download_refusal(
                &format!("{hub}/a/{digest}"),
                &reason,
                &message,
                org.as_deref(),
            );
            refuse(door, &words)
        }
        Look::Private(Err(other)) => refuse(door, &other.to_string()),
        Look::Private(Ok(meta)) => online(
            &digest,
            &hub,
            &meta,
            record,
            authorization.as_deref(),
            me.as_deref(),
            door,
        ),
    }
}

fn refuse(door: Door, words: &str) -> Result<()> {
    alert_where(door, "This app did not open", words, false);
    anyhow::bail!("{words}")
}

/// Checked in: the person may open it. Keep the lease fresh and show the
/// terms the first time.
fn online(
    digest: &str,
    hub: &str,
    meta: &Value,
    record: Option<Value>,
    authorization: Option<&str>,
    me: Option<&str>,
    door: Door,
) -> Result<()> {
    let slug = meta["org"]["slug"].as_str().unwrap_or_default().to_string();
    let org_name = meta["org"]["name"]
        .as_str()
        .filter(|n| !n.is_empty())
        .unwrap_or(&slug)
        .to_string();
    let mut record = record.unwrap_or_else(|| serde_json::json!({}));
    let key = krate_lease::lease_key();
    let held = record.get("answer").filter(|a| !a.is_null()).cloned();
    let fresh = held
        .as_ref()
        .zip(key.as_ref())
        .and_then(|(answer, key)| krate_lease::verify(answer, key).ok())
        .filter(|lease| Some(lease.principal.as_str()) == me && !lease.past_half_life(now()));
    let mut class = record["class"].as_str().unwrap_or("online").to_string();
    let mut expires = fresh.as_ref().map(|lease| lease.expires_at);
    if fresh.is_none() {
        if let (Some(authorization), false) = (authorization, slug.is_empty()) {
            match fetch_lease(hub, &slug, digest, authorization) {
                Err(reason) => {
                    forget(digest);
                    let words = org::download_refusal(
                        &format!("{hub}/a/{digest}"),
                        &reason,
                        "",
                        Some(&slug),
                    );
                    return refuse(door, &words);
                }
                Ok(Some(answer)) => {
                    if answer["class"].as_str() == Some("online") && answer["lease"].is_null() {
                        class = "online".into();
                        record["answer"] = Value::Null;
                        expires = None;
                    } else {
                        class = answer["lease"]["class"]
                            .as_str()
                            .unwrap_or("online")
                            .to_string();
                        // Kept as it came. Whether it verifies is asked when
                        // it is needed -- offline -- against the pinned key.
                        expires = key
                            .as_ref()
                            .and_then(|key| krate_lease::verify(&answer, key).ok())
                            .map(|lease| lease.expires_at);
                        if expires.is_none() {
                            class = "online".into();
                        }
                        record["answer"] = answer;
                    }
                }
                // No lease to be had right now: this open stays online-only.
                Ok(None) => {}
            }
        }
    }
    let noticed = record["noticed_class"].as_str().map(str::to_string);
    let shown_again = noticed
        .as_deref()
        .is_none_or(|was| krate_lease::class_rank(&class) < krate_lease::class_rank(was));
    if shown_again {
        tell(door, "Offline access", &notice(&org_name, &class, expires));
        record["noticed_class"] = serde_json::json!(class);
    }
    record["digest"] = serde_json::json!(digest);
    record["hub"] = serde_json::json!(hub);
    record["org"] = serde_json::json!({ "slug": slug, "name": org_name });
    record["principal"] = serde_json::json!(me);
    record["class"] = serde_json::json!(class);
    record["checked_at"] = serde_json::json!(now());
    save_record(digest, &record);
    Ok(())
}

/// No connection: open only on a lease that holds.
fn offline(digest: &str, record: &Value, me: Option<&str>, door: Door) -> Result<()> {
    let org_name = record["org"]["name"]
        .as_str()
        .unwrap_or("its organisation")
        .to_string();
    let Some(me) = me else {
        return refuse(
            door,
            &format!(
                "this is {org_name}'s private app and you are not signed in, so it does not \
                 open offline: sign in with `krate account login` while connected"
            ),
        );
    };
    let Some(answer) = record.get("answer").filter(|a| !a.is_null()) else {
        return refuse(
            door,
            &format!(
                "this is {org_name}'s private app, and it opens only with a connection: \
                 connect to the internet and open it again"
            ),
        );
    };
    // The organisation the hub named when it checked these bytes in.
    let slug = record["org"]["slug"].as_str().unwrap_or_default();
    match krate_lease::check(
        answer,
        krate_lease::lease_key().as_ref(),
        slug,
        digest,
        me,
        now(),
    ) {
        Ok(lease) => {
            eprintln!(
                "offline: opening {org_name}'s app on its offline access, which runs out {}",
                date(lease.expires_at)
            );
            Ok(())
        }
        Err(krate_lease::Refusal::Expired { expires_at }) => refuse(
            door,
            &format!(
                "this is {org_name}'s private app, and its offline access ran out on {}: \
                 connect to the internet and open it again",
                date(expires_at)
            ),
        ),
        Err(krate_lease::Refusal::OtherPerson) => refuse(
            door,
            &format!(
                "this is {org_name}'s private app, and its offline access belongs to another \
                 sign-in: connect to the internet and open it again as yourself"
            ),
        ),
        Err(why) => refuse(
            door,
            &format!(
                "this is {org_name}'s private app, and its offline access could not be checked \
                 ({}), so it needs a connection: connect to the internet and open it again",
                match why {
                    krate_lease::Refusal::NoKey =>
                        "this Krate has no hub key to check it with".to_string(),
                    krate_lease::Refusal::BadSignature => "its signature does not hold".into(),
                    krate_lease::Refusal::OtherApp => "it is for other bytes".into(),
                    krate_lease::Refusal::Malformed(what) => what,
                    _ => "it does not hold".into(),
                }
            ),
        ),
    }
}

/// The words for a refused download: one place for every door that
/// fetches (launch, MCP), as `krate run` already does. A refusal that ends
/// access also ends the lease for the bytes the link names.
pub fn refused_words(err: &krate_bundle::BundleError, url: &str) -> Option<String> {
    match err {
        krate_bundle::BundleError::HubRefused {
            reason,
            message,
            org,
            ..
        } => {
            refused_link(url, reason);
            Some(org::download_refusal(url, reason, message, org.as_deref()))
        }
        _ => None,
    }
}

/// A link's download was refused: when the refusal ends access, the lease
/// for the bytes it names goes too, exactly as a refused check-in does.
pub fn refused_link(url: &str, reason: &str) {
    if !matches!(
        reason,
        "removed" | "not-a-member" | "digest-mismatch" | "no-such-org"
    ) {
        return;
    }
    let path = url.split(['?', '#']).next().unwrap_or(url);
    if let Some(segment) = path.split("/a/").nth(1).and_then(|s| s.split('/').next()) {
        if segment.len() == 64 && segment.bytes().all(|b| b.is_ascii_hexdigit()) {
            forget(&segment.to_ascii_lowercase());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notice_names_the_date_or_says_it_needs_a_connection() {
        let days = notice("Acme", "days-7", Some(1_760_400_000));
        assert!(days.contains("Works offline until 2025-10-14 00:00 UTC, then needs a connection."));
        assert!(days.contains("If you are removed, it stops when it next connects."));
        let online = notice("Acme", "online", None);
        assert!(online.contains("needs a connection every time it opens"));
    }
}
