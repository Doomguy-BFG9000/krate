//! `krate org apps`: the company's apps on this computer (IC-953).
//!
//! The hub's `GET /orgs/<slug>/apps` is the list: for a member, exactly the
//! apps whose current release is in their audience and not withdrawn, each
//! with its version, size, what it can do and its offline class. This
//! command shows it, keeps a copy for when there is no connection, and gets
//! each app ready to open offline when its organisation allows that: the
//! release's bytes are kept in Krate's cache (and unpacked, when large
//! enough for the unpacked cache, IC-944), and its offline access is
//! fetched now, while there is a connection. Opening goes through
//! `krate run`, the one door every check sits behind; this command only
//! chooses what to open.
//!
//! Signed out, nothing is asked of a hub: the person is told how to sign
//! in, with their organisation's company sign-in when the machine policy
//! names one (`[org] slug`, IC-949 P2). That organisation is listed first.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{seg, text, words, Action, Body, Client, HubRefusal, Unreachable};
use crate::{access, github_auth};

/// What was asked of `krate org apps`.
pub struct Ask {
    pub slug: Option<String>,
    pub open: Option<String>,
    pub json: bool,
    /// Open through `krate launch`, the double-click door, rather than
    /// `krate run` in this terminal.
    pub window: bool,
    pub run_args: Vec<String>,
}

/// The company sign-in page for an organisation (IC-948 F).
fn sign_in_url(slug: &str) -> String {
    format!("https://krate.tech/login/?org={slug}")
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Where the last list of each organisation's apps is kept, for when there
/// is no connection: `~/.krate/org-apps/<slug>.json`. It names apps and
/// releases, never a credential.
fn saved_path(slug: &str) -> PathBuf {
    crate::krate_home()
        .join("org-apps")
        .join(format!("{slug}.json"))
}

/// Where a release kept ready for offline use lives: Krate's cache,
/// `org-apps/<digest>.krate`. Thrown away, it is fetched again.
fn ready_dir() -> Option<PathBuf> {
    crate::cache_root().map(|root| root.join("org-apps"))
}

fn ready_file(digest: &str) -> Option<PathBuf> {
    ready_dir().map(|dir| dir.join(format!("{digest}.krate")))
}

fn is_digest(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The list kept for this organisation, when it was kept for this sign-in.
fn load_saved(slug: &str, principal: &str) -> Option<Value> {
    let text = std::fs::read_to_string(saved_path(slug)).ok()?;
    let saved: Value = serde_json::from_str(&text).ok()?;
    (saved["principal"].as_str() == Some(principal)).then_some(saved)
}

fn save(slug: &str, saved: &Value) {
    let path = saved_path(slug);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let staged = path.with_extension(format!("json.{}", std::process::id()));
    if std::fs::write(&staged, saved.to_string()).is_ok()
        && std::fs::rename(&staged, &path).is_err()
    {
        let _ = std::fs::remove_file(&staged);
    }
}

/// Every organisation a list was kept for, for this sign-in.
fn saved_orgs(principal: &str) -> Vec<Value> {
    let dir = crate::krate_home().join("org-apps");
    let mut found: Vec<Value> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let slug = name.strip_suffix(".json")?;
            load_saved(slug, principal)
        })
        .collect();
    found.sort_by(|a, b| text(a, "slug").cmp(text(b, "slug")));
    found
}

/// The organisation is no longer this person's: its list and the bytes
/// kept for it go. The person's own app data is never touched.
fn drop_org(slug: &str) {
    if let Ok(text) = std::fs::read_to_string(saved_path(slug)) {
        if let Ok(saved) = serde_json::from_str::<Value>(&text) {
            for app in saved["apps"].as_array().into_iter().flatten() {
                if let Some(file) = app["digest"].as_str().and_then(ready_file) {
                    let _ = std::fs::remove_file(file);
                }
            }
        }
    }
    let _ = std::fs::remove_file(saved_path(slug));
}

/// Download one release to `<cache>/org-apps/<digest>.krate`, held to its
/// digest as it streams: bytes that hash to anything else are thrown away.
fn fetch_release(client: &Client, digest: &str, path: &Path) -> Result<()> {
    let url = format!("{}/a/{digest}", client.hub);
    let response = client
        .agent
        .get(&url)
        .set(
            "Authorization",
            &format!("Bearer {}", client.identity.token),
        )
        .call()
        .map_err(|err| anyhow::anyhow!("could not fetch {url}: {err}"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
    }
    let staged = path.with_extension(format!("part.{}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut reader = response
            .into_reader()
            .take(krate_bundle::HUB_BIG_LIMITS.bundle_bytes + 1);
        let mut out = std::fs::File::create(&staged)?;
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            hash.update(&buf[..n]);
            out.write_all(&buf[..n])?;
        }
        out.sync_all()?;
        if total > krate_bundle::HUB_BIG_LIMITS.bundle_bytes {
            anyhow::bail!("the release is larger than a hub release can be");
        }
        let got: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if got != digest {
            anyhow::bail!("the bytes that arrived are not the release they were asked for");
        }
        std::fs::rename(&staged, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

/// Get one listed app ready to open offline, when its organisation allows
/// offline use: its bytes kept (and unpacked when large), and its offline
/// access fetched. Answers when that access runs out, or `None`.
fn make_ready(client: &Client, org: (&str, &str), app: &Value) -> Option<u64> {
    let digest = app["digest"].as_str().filter(|d| is_digest(d))?;
    let class = app["offline"].as_str().unwrap_or("online");
    if class == "online" {
        return None;
    }
    let me = client.my_principal();
    let until = access::prefetch_lease(
        &client.hub,
        org,
        digest,
        &format!("Bearer {}", client.identity.token),
        &me,
    )
    .ok()
    .flatten()?;
    let file = ready_file(digest)?;
    if !file.is_file() && fetch_release(client, digest, &file).is_err() {
        return None;
    }
    // A large release is unpacked now, so its first open does not wait for
    // it; a small one unpacks in milliseconds and is not worth an entry.
    let large = std::fs::metadata(&file)
        .map(|meta| meta.len() >= crate::OPEN_CACHE_MIN_BYTES)
        .unwrap_or(false);
    if large {
        if let Some(root) = crate::opened_cache_root() {
            let _ = krate_bundle::open_cached(&file, &root);
        }
    }
    Some(until)
}

/// One organisation's apps, as the hub answers them now, or as this
/// computer kept them when the hub cannot be reached. `Err` is a refusal,
/// in words.
enum Listed {
    Fresh(Value),
    Saved(Value),
    Refused(String),
}

fn list_org(client: &Client, org: &Value) -> Listed {
    let slug = text(org, "slug").to_string();
    let name = Some(text(org, "name"))
        .filter(|n| !n.is_empty())
        .unwrap_or(&slug)
        .to_string();
    let me = client.my_principal();
    let path = match seg(&slug) {
        Ok(slug) => format!("/orgs/{slug}/apps"),
        Err(err) => return Listed::Refused(err.to_string()),
    };
    let answer = match client.send("GET", &path, &Body::Empty) {
        Ok(answer) => answer,
        Err(err) if err.downcast_ref::<Unreachable>().is_some() => {
            return match load_saved(&slug, &me) {
                Some(saved) => Listed::Saved(saved),
                None => Listed::Refused(format!(
                    "{name}'s apps were never listed on this computer, and the hub cannot be \
                     reached: connect to the internet and run this again"
                )),
            };
        }
        Err(err) => {
            let words = match err.downcast_ref::<HubRefusal>() {
                Some(refusal) => {
                    if matches!(
                        refusal.reason.as_str(),
                        "removed" | "not-a-member" | "no-such-org"
                    ) {
                        drop_org(&slug);
                    }
                    words(refusal, &slug, &client.me(), Action::Apps)
                }
                None => err.to_string(),
            };
            return Listed::Refused(words);
        }
    };
    let before = load_saved(&slug, &me);
    let mut apps = answer["apps"].as_array().cloned().unwrap_or_default();
    // Only what opens now is a company app on this computer.
    apps.retain(|app| app["current"].is_string());
    for app in &mut apps {
        let ready = make_ready(client, (&slug, &name), app);
        app["ready_offline_until"] = json!(ready);
    }
    // A release no longer current (withdrawn, rolled back, out of the
    // audience) is not kept ready any more.
    let now_digests: Vec<&str> = apps.iter().filter_map(|a| a["digest"].as_str()).collect();
    for old in before
        .iter()
        .flat_map(|b| b["apps"].as_array().cloned().unwrap_or_default())
    {
        if let Some(digest) = old["digest"].as_str().filter(|d| !now_digests.contains(d)) {
            if let Some(file) = ready_file(digest) {
                let _ = std::fs::remove_file(file);
            }
        }
    }
    let saved = json!({
        "v": 1,
        "slug": slug,
        "name": name,
        "role": org["role"],
        "principal": me,
        "hub": client.hub,
        "saved_at": now(),
        "apps": apps,
    });
    save(&slug, &saved);
    Listed::Fresh(saved)
}

/// What one app can do, in a line: the IC-950 words, or that it asks for
/// nothing.
fn can_do(app: &Value) -> String {
    let words: Vec<&str> = app["capabilities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["words"].as_str().or(c["cap"].as_str()))
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        "asks for nothing on this computer".to_string()
    } else {
        words.join("; ")
    }
}

fn size_words(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else {
        format!("{} KB", bytes.div_ceil(1000).max(1))
    }
}

pub fn run(hub: String, ask: Ask) -> Result<u8> {
    let policy_org = crate::fleet::policy_org().map(str::to_string);
    // The organisation's own name, as the policy gives it, for what a
    // person reads: "Acme Labs", not "acme".
    let policy_name = crate::fleet::policy().and_then(|p| p.name.clone());
    let Some(identity) = github_auth::current() else {
        let first = ask.slug.clone().or(policy_org.clone());
        if ask.json {
            println!(
                "{}",
                json!({
                    "v": 1, "signed_in": false, "policy_org": policy_org,
                    "policy_name": policy_name,
                    "sign_in": first.as_deref().map(sign_in_url), "orgs": [],
                })
            );
            return Ok(0);
        }
        match first {
            Some(slug) => anyhow::bail!(
                "sign in to see {}'s apps: open {} to use your company sign-in, or run \
                 `krate account login`, then run this again",
                policy_name
                    .as_deref()
                    .filter(|_| Some(&slug) == policy_org.as_ref())
                    .unwrap_or(&slug),
                sign_in_url(&slug)
            ),
            None => anyhow::bail!(
                "you are not signed in: run `krate account login`, then run this again"
            ),
        }
    };
    let client = Client::new(hub, identity);
    let me = client.my_principal();

    if let Some(app_id) = ask.open.as_deref() {
        return open(&client, &ask, policy_org.as_deref(), app_id);
    }

    // Which organisations: the one named, or every one this person is in.
    let mut offline = false;
    let mut orgs: Vec<Value> = match client.send("GET", "/orgs", &Body::Empty) {
        Ok(answer) => answer
            .as_array()
            .cloned()
            .or_else(|| answer["orgs"].as_array().cloned())
            .unwrap_or_default(),
        Err(err) if err.downcast_ref::<Unreachable>().is_some() => {
            offline = true;
            saved_orgs(&me)
                .into_iter()
                .map(|s| json!({ "slug": s["slug"], "name": s["name"], "role": s["role"] }))
                .collect()
        }
        Err(err) => match err.downcast_ref::<HubRefusal>() {
            Some(refusal) => anyhow::bail!(
                "{}",
                words(
                    refusal,
                    ask.slug.as_deref().unwrap_or(""),
                    &client.me(),
                    Action::List
                )
            ),
            None => return Err(err),
        },
    };
    if let Some(slug) = ask.slug.as_deref() {
        let slug = slug.trim().to_ascii_lowercase();
        orgs.retain(|o| text(o, "slug") == slug);
        if orgs.is_empty() {
            // Asked for by name: the hub answers for it, whatever /orgs said.
            orgs.push(json!({ "slug": slug, "name": slug }));
        }
    }
    // The organisation this computer belongs to comes first.
    orgs.sort_by_key(|o| Some(text(o, "slug")) != policy_org.as_deref());

    let mut listed = Vec::new();
    let mut refusals = Vec::new();
    for org in &orgs {
        let slug = text(org, "slug").to_string();
        match list_org(&client, org) {
            Listed::Fresh(saved) => listed.push((saved, false)),
            Listed::Saved(saved) => listed.push((saved, true)),
            Listed::Refused(words) => refusals.push((slug, words)),
        }
    }

    if ask.json {
        let orgs: Vec<Value> = listed
            .iter()
            .map(|(saved, stale)| {
                json!({
                    "slug": saved["slug"], "name": saved["name"], "role": saved["role"],
                    "first": saved["slug"].as_str() == policy_org.as_deref(),
                    "from": if *stale { "saved" } else { "hub" },
                    "saved_at": saved["saved_at"],
                    "apps": saved["apps"],
                })
            })
            .collect();
        let refused: Vec<Value> = refusals
            .iter()
            .map(|(slug, words)| json!({ "slug": slug, "words": words }))
            .collect();
        println!(
            "{}",
            json!({
                "v": 1, "signed_in": true, "me": me, "policy_org": policy_org,
                "offline": offline || listed.iter().any(|(_, stale)| *stale),
                "orgs": orgs, "refused": refused,
            })
        );
        return Ok(0);
    }

    if listed.is_empty() && refusals.is_empty() {
        match policy_org.as_deref() {
            Some(slug) => println!(
                "you are not in {slug} yet: ask an owner or admin of {slug} to invite {}",
                client.me()
            ),
            None => println!("you're in no organisations yet, so there are no company apps here"),
        }
        return Ok(0);
    }
    for (saved, stale) in &listed {
        let slug = text(saved, "slug");
        let name = text(saved, "name");
        let apps = saved["apps"].as_array().cloned().unwrap_or_default();
        let mine = if Some(slug) == policy_org.as_deref() {
            ", this computer's organisation"
        } else {
            ""
        };
        println!(
            "{name} ({slug}{mine}): {} app{}",
            apps.len(),
            if apps.len() == 1 { "" } else { "s" }
        );
        if *stale {
            println!(
                "  No connection: this is the list from {}.",
                super::when(&saved["saved_at"])
            );
        }
        for app in &apps {
            let ready = match app["ready_offline_until"].as_u64() {
                Some(until) => format!("works offline until {}", super::when(&json!(until))),
                None if app["offline"].as_str() == Some("online") => {
                    "needs a connection to open".to_string()
                }
                None => "opens with a connection".to_string(),
            };
            println!(
                "  {} {}  {}  {}  {ready}",
                text(app, "name"),
                text(app, "version"),
                text(app, "app_id"),
                size_words(app["size"].as_u64().unwrap_or(0)),
            );
            println!("      can: {}", can_do(app));
        }
        if let Some(app) = apps.first() {
            println!(
                "  Open one: krate org apps {slug} --open {}",
                text(app, "app_id")
            );
        }
    }
    for (slug, words) in &refusals {
        println!("{slug}: {words}");
    }
    Ok(if listed.is_empty() { 1 } else { 0 })
}

/// `--open <app id>`: what the app's link opens now, through `krate run`.
/// Online, the hub says which release that is (a rollback opens the older
/// one); offline, the list this computer kept says, and only bytes kept
/// ready here can open, on their offline access.
fn open(client: &Client, ask: &Ask, policy_org: Option<&str>, app_id: &str) -> Result<u8> {
    let me = client.my_principal();
    let app_id = app_id.trim().to_ascii_lowercase();
    let slug = match ask.slug.clone().or_else(|| policy_org.map(str::to_string)) {
        Some(slug) => slug.trim().to_ascii_lowercase(),
        None => {
            let orgs = saved_orgs(&me);
            let with: Vec<&Value> = orgs
                .iter()
                .filter(|o| {
                    o["apps"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|x| text(x, "app_id") == app_id))
                })
                .collect();
            match with.as_slice() {
                [one] => text(one, "slug").to_string(),
                _ => anyhow::bail!(
                    "say which organisation {app_id} is in: krate org apps <organisation> --open \
                     {app_id}"
                ),
            }
        }
    };
    let path = format!("/orgs/{}/apps/{}/current", seg(&slug)?, seg(&app_id)?);
    let link = format!("{}{path}", client.hub);
    let digest = match client.send("GET", &path, &Body::Empty) {
        Ok(answer) => answer["digest"].as_str().map(str::to_string),
        Err(err) if err.downcast_ref::<Unreachable>().is_some() => {
            let kept = load_saved(&slug, &me).and_then(|saved| {
                saved["apps"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|a| text(a, "app_id") == app_id)
                    .and_then(|a| a["digest"].as_str().map(str::to_string))
            });
            match kept {
                Some(digest) => Some(digest),
                None => anyhow::bail!(
                    "{app_id} was never listed on this computer, and the hub cannot be \
                     reached: connect to the internet and open it again"
                ),
            }
        }
        Err(err) => {
            return Err(match err.downcast_ref::<HubRefusal>() {
                Some(refusal) => {
                    let said = match refusal.reason.as_str() {
                        "release-withdrawn" | "not-in-audience" => {
                            format!("{app_id} does not open: {}", refusal.message)
                        }
                        "digest-mismatch" => {
                            format!("{slug} has no release of {app_id} that opens now")
                        }
                        _ => words(refusal, &slug, &client.me(), Action::Apps),
                    };
                    anyhow::anyhow!("{said}")
                }
                None => err,
            });
        }
    };
    // The bytes kept ready here, when they are what opens now; otherwise
    // the app's own link, fetched with every check a download makes.
    let kept = digest
        .as_deref()
        .filter(|d| is_digest(d))
        .and_then(ready_file)
        .filter(|file| file.is_file());
    let target = match kept {
        Some(file) => file.display().to_string(),
        None => link,
    };
    let exe = std::env::current_exe().context("could not find this Krate to open the app")?;
    let mut run = std::process::Command::new(exe);
    if ask.window {
        run.arg("launch").arg(&target);
    } else {
        run.arg("run").arg(&target);
        if target.starts_with("http://") {
            // A hub on this machine (a pinned one may only be loopback over
            // http); `krate run` refuses plain http unless told.
            run.arg("--insecure-http");
        }
        run.args(&ask.run_args);
    }
    let status = run
        .status()
        .context("could not start Krate to open the app")?;
    Ok(status.code().unwrap_or(1).clamp(0, 255) as u8)
}
