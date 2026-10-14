//! `krate org connection ...`: an organisation's company connections
//! (IC-952).
//!
//! A connection is an API's address and credential, set up once by an owner
//! or admin, that the organisation's private apps use without ever holding
//! the credential: an app declares `net.connection:<name>`, and Krate adds
//! the credential to that app's requests to the address.
//!
//! The credential is never an argument, because an argument stays in the
//! shell's history and shows in the list of running processes. It is read
//! from a file (`--value-file`) or typed at a prompt that does not show it,
//! sent once, and never printed back: the hub keeps it encrypted and shows
//! only who set it and when.

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Subcommand;

use super::{seg, spoken_principal, text, when, worded, Action, Body, Client};

#[derive(Debug, Subcommand)]
pub enum ConnectionCommand {
    /// The organisation's connections: each one's address, what it allows,
    /// who and which apps may use it, and when its credential was set. Never
    /// the credential itself.
    List { slug: String },
    /// Set up a connection. The credential's value is read from
    /// --value-file, or asked for at a prompt that does not show it.
    Create {
        slug: String,
        /// Its name, which apps ask for as `net.connection:<name>`: 2 to 39
        /// lower-case letters, digits and dashes, like finance-api.
        name: String,
        /// The address it covers: https, a host and a path, like
        /// https://api.acme.com/finance. Requests under it get the credential.
        #[arg(long)]
        base: String,
        /// The credential is this header, like Authorization or X-Api-Key;
        /// its value is read from --value-file or the prompt.
        #[arg(long, value_name = "NAME", conflicts_with = "basic")]
        header: Option<String>,
        /// The credential is a user and password, sent as Basic; this is the
        /// user, and the password is read from --value-file or the prompt.
        #[arg(long, value_name = "USER")]
        basic: Option<String>,
        /// A file holding the header's value or the password, on one line.
        #[arg(long, value_name = "FILE")]
        value_file: Option<PathBuf>,
        /// The methods apps may use, comma-separated. GET alone unless given.
        #[arg(long, value_delimiter = ',', default_value = "GET")]
        allow: Vec<String>,
        /// The paths under the address apps may reach, comma-separated, like
        /// /invoices,/rates. All of it unless given.
        #[arg(long, value_delimiter = ',')]
        paths: Vec<String>,
        /// Only members of these groups may use it, comma-separated.
        /// Everyone in the organisation unless given.
        #[arg(long, value_delimiter = ',')]
        groups: Vec<String>,
        /// Only these apps may use it, by the app id in their manifest,
        /// comma-separated. Any of the organisation's apps unless given.
        #[arg(long, value_delimiter = ',')]
        apps: Vec<String>,
    },
    /// Give a connection a new credential. Every app that uses it gets the
    /// new one the next time it opens.
    Replace {
        slug: String,
        name: String,
        /// The credential is this header; its value comes from --value-file
        /// or the prompt.
        #[arg(long, value_name = "NAME", conflicts_with = "basic")]
        header: Option<String>,
        /// The credential is a user and password, sent as Basic.
        #[arg(long, value_name = "USER")]
        basic: Option<String>,
        #[arg(long, value_name = "FILE")]
        value_file: Option<PathBuf>,
    },
    /// Remove a connection and its credential. Apps that use it are refused
    /// it from their next open.
    Remove { slug: String, name: String },
}

/// The value of a credential: one line from a file, or typed at a prompt
/// that does not echo. Never from an argument.
fn read_value(file: Option<&PathBuf>, what: &str) -> Result<String> {
    let line = match file {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("could not read {}", path.display()))?,
        None => {
            let stdin = std::io::stdin();
            if stdin.is_terminal() {
                eprint!("{what} (it is not shown as you type, and never printed): ");
                let _ = std::io::stderr().flush();
            }
            let _quiet = Quiet::enter();
            let mut line = String::new();
            stdin
                .lock()
                .read_line(&mut line)
                .context("could not read the value")?;
            if stdin.is_terminal() {
                eprintln!();
            }
            line
        }
    };
    let value = line.trim_end_matches(['\r', '\n']).to_string();
    if value.trim().is_empty() {
        anyhow::bail!("nothing was sent: the {what} was empty");
    }
    Ok(value)
}

/// Echo off for as long as this lives, when stdin is a terminal.
struct Quiet {
    #[cfg(unix)]
    previous: Option<libc::termios>,
}

impl Quiet {
    fn enter() -> Self {
        #[cfg(unix)]
        {
            if !std::io::stdin().is_terminal() {
                return Self { previous: None };
            }
            // SAFETY: tcgetattr fills a termios we own; the fd is stdin.
            unsafe {
                let mut previous: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(libc::STDIN_FILENO, &mut previous) != 0 {
                    return Self { previous: None };
                }
                let mut quiet = previous;
                quiet.c_lflag &= !libc::ECHO;
                if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &quiet) != 0 {
                    return Self { previous: None };
                }
                Self {
                    previous: Some(previous),
                }
            }
        }
        #[cfg(not(unix))]
        {
            Self {}
        }
    }
}

#[cfg(unix)]
impl Drop for Quiet {
    fn drop(&mut self) {
        if let Some(previous) = self.previous {
            // SAFETY: restoring the termios captured in `enter`.
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &previous);
            }
        }
    }
}

/// The credential part of a request body.
fn credential(
    header: Option<&str>,
    basic: Option<&str>,
    value_file: Option<&PathBuf>,
) -> Result<serde_json::Value> {
    match (header, basic) {
        (Some(name), None) => {
            let value = read_value(value_file, &format!("{name} value"))?;
            Ok(serde_json::json!({ "header": { "name": name, "value": value } }))
        }
        (None, Some(user)) => {
            let password = read_value(value_file, &format!("password for {user}"))?;
            Ok(serde_json::json!({ "basic": { "user": user, "password": password } }))
        }
        _ => anyhow::bail!(
            "say what the credential is: --header <name> (like --header Authorization), or \
             --basic <user>; its value is read from --value-file or asked for"
        ),
    }
}

/// The entry side of the credential seam (F-316): what leaves this machine
/// for the hub. Today the credential goes as typed, over TLS, and the hub
/// seals it at rest. Under the organisation keyring this seals it to the
/// organisation's public key first, so the hub only ever holds the blob.
fn seal_for_hub(body: serde_json::Value) -> serde_json::Value {
    body
}

fn spoken_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn strings(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// One connection, as `list` and `create` print it.
fn describe(c: &serde_json::Value) -> Vec<String> {
    let methods = strings(&c["allow"]["methods"]);
    let paths = strings(&c["allow"]["paths"]);
    let audience = strings(&c["audience"]);
    let apps = strings(&c["apps"]);
    let reach = if paths == ["/"] {
        "anything under it".to_string()
    } else {
        spoken_list(&paths)
    };
    vec![
        format!("  {}  {}", text(c, "name"), text(c, "base")),
        format!("    allows {} on {reach}", spoken_list(&methods)),
        format!(
            "    for {}, in {}",
            if audience.is_empty() {
                "everyone".to_string()
            } else {
                format!(
                    "the {} group{}",
                    spoken_list(&audience),
                    if audience.len() == 1 { "" } else { "s" }
                )
            },
            if apps.is_empty() {
                "any of the organisation's apps".to_string()
            } else {
                spoken_list(&apps)
            }
        ),
        format!(
            "    credential: the {} header, set by {} {}; used {} times today, {} in 30 days",
            text(&c["credential"], "header"),
            spoken_principal(&c["credential"]["set_by"]),
            when(&c["credential"]["set_at"]),
            c["uses"]["today"].as_u64().unwrap_or(0),
            c["uses"]["last_30_days"].as_u64().unwrap_or(0),
        ),
    ]
}

pub(super) fn run(client: &Client, me: &str, command: ConnectionCommand) -> Result<u8> {
    match command {
        ConnectionCommand::List { slug } => {
            let answer = worded(
                client.send(
                    "GET",
                    &format!("/orgs/{}/connections", seg(&slug)?),
                    &Body::Empty,
                ),
                &slug,
                me,
                Action::Connection,
            )?;
            let all = answer["connections"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if all.is_empty() {
                println!(
                    "{slug} has no connections yet. An owner or admin sets one up with \
                     `krate org connection create {slug} <name> --base <https address> \
                     --header <name>`."
                );
                return Ok(0);
            }
            for c in &all {
                for line in describe(c) {
                    println!("{line}");
                }
            }
            Ok(0)
        }
        ConnectionCommand::Create {
            slug,
            name,
            base,
            header,
            basic,
            value_file,
            allow,
            paths,
            groups,
            apps,
        } => {
            let name = name.trim().to_ascii_lowercase();
            let mut body = seal_for_hub(credential(
                header.as_deref(),
                basic.as_deref(),
                value_file.as_ref(),
            )?);
            body["name"] = serde_json::json!(name);
            body["base"] = serde_json::json!(base);
            body["allow"] = serde_json::json!({
                "methods": allow,
                "paths": if paths.is_empty() { vec!["/".to_string()] } else { paths },
            });
            if !groups.is_empty() {
                body["audience"] = serde_json::json!({ "groups": groups });
            }
            if !apps.is_empty() {
                body["apps"] = serde_json::json!({ "ids": apps });
            }
            let answer = worded(
                client.send(
                    "POST",
                    &format!("/orgs/{}/connections", seg(&slug)?),
                    &Body::Json(body),
                ),
                &slug,
                me,
                Action::Connection,
            )?;
            println!("set up the {name} connection in {slug}:");
            for line in describe(&answer["connection"]) {
                println!("{line}");
            }
            println!("An app uses it by declaring this in its manifest.toml:");
            println!("  [[capabilities]]");
            println!("  cap = \"net.connection:{name}\"");
            println!("  rationale = \"<what it reads or sends>\"");
            println!("The app never sees the credential: Krate adds it to the app's requests.");
            Ok(0)
        }
        ConnectionCommand::Replace {
            slug,
            name,
            header,
            basic,
            value_file,
        } => {
            let name = name.trim().to_ascii_lowercase();
            let body = seal_for_hub(credential(
                header.as_deref(),
                basic.as_deref(),
                value_file.as_ref(),
            )?);
            worded(
                client.send(
                    "PUT",
                    &format!(
                        "/orgs/{}/connections/{}/credential",
                        seg(&slug)?,
                        seg(&name)?
                    ),
                    &Body::Json(body),
                ),
                &slug,
                me,
                Action::Connection,
            )?;
            println!(
                "{name} has a new credential. Every app that uses it gets the new one the next \
                 time it opens; the old one is gone from the hub."
            );
            Ok(0)
        }
        ConnectionCommand::Remove { slug, name } => {
            let name = name.trim().to_ascii_lowercase();
            worded(
                client.send(
                    "DELETE",
                    &format!("/orgs/{}/connections/{}", seg(&slug)?, seg(&name)?),
                    &Body::Empty,
                ),
                &slug,
                me,
                Action::Connection,
            )?;
            println!(
                "removed the {name} connection from {slug}, with its credential. Apps that \
                 used it are refused it from their next open."
            );
            Ok(0)
        }
    }
}
