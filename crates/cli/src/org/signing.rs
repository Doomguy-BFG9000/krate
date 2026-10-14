//! Signing nobody has to think about (IC-958) -- this computer's part.
//!
//! - **Making the organisation's signing key** when its first owner sets it
//!   up ([`ring_init`]): sealed for this computer and the recovery code,
//!   registered as the organisation's, never shown.
//! - **Permission to publish** ([`permission`]): the first time someone
//!   publishes to an organisation from this computer, Krate makes its
//!   release key here, asks the organisation, and waits while an owner's or
//!   admin's computer approves it in the background. Nobody copies a file.
//! - **The background share** ([`duties`]): on an owner's or admin's
//!   computer, pass the keyring and the signing key on to new devices,
//!   approve waiting publishers, withdraw the release keys of people who
//!   left. Runs after every `krate org` command, while an organisation's app
//!   runs (`krate run`), and from Studio's periodic check (`krate org sync`).
//!   Never a daemon; offline is normal and says nothing.
//!
//! The hub's side is cloud/worker/src/org-signing.js.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use krate_orgkeys::agent::{Agent, AgentError, Duties, RingStatus};

use super::{github_auth, release_dir, seg, Action, Body, Client, HubRefusal};

/// `krate publish --org` exits with this while it waits for the
/// organisation to approve this person as a publisher: nothing was sent,
/// and the same command finishes the job once they are approved. (75 is
/// EX_TEMPFAIL: try again later.)
pub const EXIT_WAITING: u8 = 75;

/// How long `krate publish --org` waits for the approval, by default.
const DEFAULT_WAIT: Duration = Duration::from_secs(60);

/// What this computer may do about publishing to an organisation.
pub enum Permission {
    /// Ready: the release key and the organisation's permission slip.
    Ready { key: PathBuf, delegation: PathBuf },
    /// Not approved yet; the words were said.
    Waiting,
    /// The organisation does not sign automatically (its key is kept by
    /// hand, or the hub is older): the expert path applies.
    ByHand,
}

fn agent(
    hub: &str,
    slug: &str,
    authorization: Option<String>,
    principal: Option<&str>,
) -> Result<Agent> {
    Ok(Agent::new(
        hub,
        slug,
        authorization,
        crate::access::org_device_key()?,
        &crate::access::device_name(principal),
    ))
}

fn agent_of(client: &Client, slug: &str) -> Result<Agent> {
    agent(
        &client.hub,
        slug,
        Some(format!("Bearer {}", client.identity.token)),
        Some(&client.my_principal()),
    )
}

/// Make the organisation's signing key on this computer, an owner's.
pub(super) fn ring_init(client: &Client, slug: &str) -> Result<()> {
    agent_of(client, slug)?
        .signing_init()
        .map(|_| ())
        .map_err(|err| anyhow::anyhow!("{err}"))
}

/// This computer's share for one organisation, quietly. What it did, or
/// nothing when there was nothing to do, nothing it may do, or no hub.
pub fn duties(
    hub: &str,
    slug: &str,
    authorization: Option<String>,
    principal: Option<&str>,
) -> Option<Duties> {
    let agent = agent(hub, slug, authorization, principal).ok()?;
    // The keyring first: it registers this device, and passes the team's
    // data key on.
    let _ = agent.refresh(None);
    match agent.signing_refresh() {
        Ok((RingStatus::Holds(_), duties)) => Some(duties),
        _ => None,
    }
}

/// One line for what the background share did, if anything.
pub fn said(slug: &str, duties: &Duties) -> Option<String> {
    let mut parts = Vec::new();
    if !duties.approved.is_empty() {
        let who: Vec<String> = duties
            .approved
            .iter()
            .map(|p| p.split_once(':').map_or(p.as_str(), |(_, v)| v).to_string())
            .collect();
        parts.push(format!("approved {} to publish to {slug}", list(&who)));
    }
    if duties.withdrawn > 0 {
        parts.push(format!(
            "withdrew the publishing rights of {} who left {slug}",
            if duties.withdrawn == 1 {
                "someone".to_string()
            } else {
                format!("{} people", duties.withdrawn)
            }
        ));
    }
    if duties.ring_grants > 0 {
        parts.push(format!(
            "let {} new owner or admin computer{} sign for {slug}",
            duties.ring_grants,
            if duties.ring_grants == 1 { "" } else { "s" }
        ));
    }
    (!parts.is_empty()).then(|| format!("This computer {}.", parts.join("; ")))
}

fn list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [first, rest @ ..] => format!("{first} and {} others", rest.len()),
    }
}

/// After a `krate org` command for `slug`: do the share, and say what it
/// did on stderr, so a command's own output (`--json`) stays its own.
pub(super) fn after_command(client: &Client, slug: &str) {
    if let Some(done) = duties(
        &client.hub,
        slug,
        Some(format!("Bearer {}", client.identity.token)),
        Some(&client.my_principal()),
    ) {
        if let Some(line) = said(slug, &done) {
            eprintln!("{line}");
        }
    }
}

/// The share for every organisation this person is in: `krate org sync`,
/// what Studio runs every minute while it is open.
pub(super) fn sync_all(client: &Client, quiet: bool) -> Result<u8> {
    let orgs = client.send("GET", "/orgs", &Body::Empty)?;
    for org in orgs.as_array().into_iter().flatten() {
        let Some(slug) = org["slug"].as_str() else {
            continue;
        };
        let done = duties(
            &client.hub,
            slug,
            Some(format!("Bearer {}", client.identity.token)),
            Some(&client.my_principal()),
        );
        if let Some(line) = done.as_ref().and_then(|d| said(slug, d)) {
            println!("{line}");
        } else if !quiet {
            println!("{slug}: nothing waiting for this computer");
        }
    }
    Ok(0)
}

/// `krate org setup <slug>` (what Studio runs for krate://org/setup): make
/// this computer one of the organisation's devices. An owner of an
/// organisation with no keys yet makes them here; otherwise this computer
/// registers, takes what it may hold from the devices that already do,
/// and does its share.
pub(super) fn setup(client: &Client, slug: &str) -> Result<u8> {
    let agent = agent_of(client, slug)?;
    let me = client.me();
    let words = |err: AgentError| match err {
        AgentError::Refused {
            reason, message, ..
        } => anyhow::anyhow!(super::words(
            &HubRefusal { reason, message },
            slug,
            &me,
            Action::Show
        )),
        AgentError::Offline => {
            anyhow::anyhow!("could not reach Krate to set up {slug}: try again when online")
        }
        other => anyhow::anyhow!("{other}"),
    };
    let view = client.send("GET", &format!("/orgs/{}", seg(slug)?), &Body::Empty)?;
    let name = view["org"]["name"]
        .as_str()
        .filter(|n| !n.is_empty())
        .unwrap_or(slug)
        .to_string();
    let owner = view["me"]["role"].as_str() == Some("owner");
    let keys = agent.refresh(None).map_err(words)?;
    if matches!(keys, krate_orgkeys::agent::Status::NoKey) && owner {
        super::keys_init(client, slug)?;
    }
    let (ring, _) = agent.signing_refresh().map_err(words)?;
    let ring = match ring {
        RingStatus::NoRing if owner => {
            ring_init(client, slug)?;
            agent.signing_refresh().map_err(words)?.0
        }
        other => other,
    };
    let keys = agent.refresh(None).map_err(words)?;
    let holds_keys = matches!(keys, krate_orgkeys::agent::Status::Holds(_));
    match ring {
        RingStatus::Holds(_) => println!(
            "This computer is set up for {name}: it signs {name}'s apps and passes {name}'s keys \
             to new devices, by itself, while Krate is open."
        ),
        RingStatus::Waiting => println!(
            "This computer is waiting for another owner's computer or browser to let it sign for \
             {name}. That happens the next time one of them opens Krate."
        ),
        _ if holds_keys => println!("This computer is set up for {name}."),
        _ => println!(
            "This computer is waiting for {name}'s keys. Another member's computer or browser \
             passes them on the next time it opens Krate."
        ),
    }
    Ok(0)
}

static BACKGROUND: Mutex<Vec<std::thread::JoinHandle<()>>> = Mutex::new(Vec::new());

/// The share for an organisation whose app is opening, beside the app.
pub fn in_background(
    hub: String,
    slug: String,
    authorization: Option<String>,
    principal: Option<String>,
) {
    let handle = std::thread::spawn(move || {
        let _ = duties(&hub, &slug, authorization, principal.as_deref());
    });
    if let Ok(mut all) = BACKGROUND.lock() {
        all.push(handle);
    }
}

/// Before the process ends: give the share up to `limit` to finish, so a
/// short run still does it. It is safe to cut: every step is idempotent.
pub fn finish_background(limit: Duration) {
    let handles: Vec<_> = BACKGROUND
        .lock()
        .map(|mut all| all.drain(..).collect())
        .unwrap_or_default();
    let until = Instant::now() + limit;
    for handle in handles {
        while !handle.is_finished() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(25));
        }
        if handle.is_finished() {
            let _ = handle.join();
        }
    }
}

fn wait_limit() -> Duration {
    std::env::var("KRATE_PUBLISH_WAIT_SECONDS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_WAIT)
}

fn public_hex(path: &std::path::Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    let key = krate_bundle::signing::SigningKey::from_pkcs8(&bytes)
        .map_err(|err| anyhow::anyhow!("{}: {err}", path.display()))?;
    Ok(key
        .public_key()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// The marker that says this computer's permission for `slug` was set up
/// by Krate itself, so signing with it says nothing about keys.
pub fn automatic(slug: &str) -> bool {
    release_dir(slug).is_ok_and(|dir| dir.join("automatic").is_file())
}

/// Permission to publish to `slug` from this computer, asked for and
/// waited on in words. The release key is made here the first time.
pub fn permission(hub: &str, identity: &github_auth::Identity, slug: &str) -> Result<Permission> {
    let client = Client::new(hub.trim_end_matches('/').to_string(), identity.clone());
    let me = client.me();
    let settings = match client.send(
        "GET",
        &format!("/orgs/{}/settings/publishing", seg(slug)?),
        &Body::Empty,
    ) {
        Ok(settings) => settings,
        Err(err) => match err.downcast_ref::<HubRefusal>() {
            // Words the person needs (not a member, removed) are said now.
            Some(refusal) if refusal.reason != "bad-request" => {
                anyhow::bail!(super::words(refusal, slug, &me, Action::Publish))
            }
            // A hub from before automatic signing.
            _ => return Ok(Permission::ByHand),
        },
    };
    if settings["signing"].as_bool() != Some(true) {
        return Ok(Permission::ByHand);
    }
    let org_name = client
        .send("GET", &format!("/orgs/{}", seg(slug)?), &Body::Empty)
        .ok()
        .and_then(|v| v["org"]["name"].as_str().map(str::to_string))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| slug.to_string());
    let dir = release_dir(slug)?;
    let key = dir.join("release.pkcs8");
    if !key.is_file() {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("could not make {}", dir.display()))?;
        let document = krate_bundle::signing::SigningKey::generate_pkcs8().map_err(|err| {
            anyhow::anyhow!("could not get this computer ready to publish: {err}")
        })?;
        super::write_private_key(&key, &document)?;
    }
    let public = public_hex(&key)?;
    println!("Getting you ready to publish to {org_name}...");
    let agent = agent_of(&client, slug)?;
    let refused = |err: AgentError| {
        match err {
        AgentError::Refused { reason, message, .. } => anyhow::anyhow!(super::words(
            &HubRefusal { reason, message },
            slug,
            &me,
            Action::Publish
        )),
        AgentError::Offline => anyhow::anyhow!("could not reach Krate to ask {org_name} for you: nothing was sent. Try again when online"),
        other => anyhow::anyhow!("{other}"),
    }
    };
    let mut publisher = agent
        .ask_to_publish(
            &public,
            &crate::access::device_name(Some(&client.my_principal())),
        )
        .map_err(refused)?;
    let limit = wait_limit();
    let started = Instant::now();
    let mut told = false;
    loop {
        match publisher.state.as_str() {
            "signed" => break,
            "revoked" | "revoke-due" | "closed" => {
                // This computer's key was withdrawn: the next publish makes
                // a new one and asks again.
                let _ = std::fs::remove_file(&key);
                let _ = std::fs::remove_file(dir.join("delegation.json"));
                anyhow::bail!(
                    "{org_name} no longer lets this computer's earlier permission publish, so nothing was sent: run the same command again to ask {org_name} afresh"
                );
            }
            _ => {}
        }
        // An owner's or admin's own computer approves itself.
        let _ = agent.signing_refresh();
        publisher = agent.publisher(&public).map_err(refused)?;
        if publisher.state == "signed" {
            break;
        }
        let words = waiting_words(&org_name, slug, &publisher);
        if !told {
            println!("{words}");
            told = true;
        }
        if started.elapsed() >= limit {
            println!(
                "Nothing was sent yet. You are approved as soon as one of them has Krate open; then run the same command again."
            );
            return Ok(Permission::Waiting);
        }
        std::thread::sleep(
            Duration::from_millis(1500)
                .min(limit.saturating_sub(started.elapsed()))
                .max(Duration::from_millis(100)),
        );
        publisher = agent.publisher(&public).map_err(refused)?;
    }
    let text = publisher.delegation.ok_or_else(|| {
        anyhow::anyhow!(
            "{org_name} approved you, but its answer was incomplete: run the same command again"
        )
    })?;
    let slip: krate_bundle::delegation::SignedDelegation =
        serde_json::from_str(&text).map_err(|_| {
            anyhow::anyhow!("{org_name}'s approval could not be read: run the same command again")
        })?;
    if !slip.delegation.key.eq_ignore_ascii_case(&public) {
        anyhow::bail!("{org_name}'s approval is for another computer: run the same command again");
    }
    let delegation = dir.join("delegation.json");
    std::fs::write(&delegation, &text)
        .with_context(|| format!("could not write {}", delegation.display()))?;
    std::fs::write(dir.join("automatic"), b"set up by Krate (IC-958)\n")
        .with_context(|| format!("could not write in {}", dir.display()))?;
    if told {
        println!("Approved: you can publish to {org_name}.");
    }
    Ok(Permission::Ready { key, delegation })
}

/// "Waiting for an owner's computer to approve you as a publisher -- Alice
/// and 2 others can." and its by-hand and nobody-yet forms.
pub fn waiting_words(org_name: &str, slug: &str, p: &krate_orgkeys::agent::Publisher) -> String {
    if p.by_hand && p.state == "waiting" {
        return format!(
            "Waiting for an owner or admin of {org_name} to approve you as a publisher: https://krate.tech/org/?o={slug}&tab=people"
        );
    }
    if p.can_sign.is_empty() {
        return format!(
            "Waiting for an owner of {org_name} to open Krate, so their computer can approve you as a publisher."
        );
    }
    format!(
        "Waiting for an owner's computer to approve you as a publisher -- {} can.",
        list(&p.can_sign)
    )
}
