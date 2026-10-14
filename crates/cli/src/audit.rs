//! The open record: one line per app opened, or refused, on this computer
//! (IC-949, FLEET-CONTRACT A).
//!
//! Always on and local only: it is never sent anywhere. With an
//! organisation's policy in force it goes to `[audit] path`, else
//! `/var/log/krate/opens.jsonl` (macOS, Linux) or
//! `%ProgramData%\Krate\logs\opens.jsonl` (Windows); when the person running
//! Krate cannot write there, to `<krate home>/logs/opens.jsonl`, and
//! `krate policy` says so. With no policy it is `<krate home>/logs/
//! opens.jsonl`, the person's own history.
//!
//! One JSON object per line, appended with a single write. No paths to the
//! person's documents, no app data, no prompts. Past 10 MiB the file becomes
//! `opens.1.jsonl` and a new one starts; one generation is kept.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// The size past which the record is rotated.
pub const ROTATE_AT: u64 = 10 * 1024 * 1024;

/// A refusal at a door: what the person saw, and the stable code the open
/// record carries. Its `Display` is the words alone, so it reads exactly as
/// the plain error it replaces.
#[derive(Debug, Clone)]
pub struct Refused {
    pub reason: String,
    pub words: String,
}

impl Refused {
    pub fn new(reason: &str, words: String) -> Self {
        Self {
            reason: reason.to_string(),
            words,
        }
    }
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.words)
    }
}

impl std::error::Error for Refused {}

/// Which door an app came through, as the record names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordDoor {
    Window,
    Terminal,
    Mcp,
    Player,
}

impl RecordDoor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::Terminal => "terminal",
            Self::Mcp => "mcp",
            Self::Player => "player",
        }
    }

    /// How the access machinery shows words at this door.
    pub fn access(self) -> crate::access::Door {
        match self {
            Self::Window | Self::Player => crate::access::Door::Window,
            Self::Terminal | Self::Mcp => crate::access::Door::Terminal,
        }
    }
}

/// Set by the Player's own open path, so its runs are recorded as such.
pub static OPENED_BY_PLAYER: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The door a `krate run` came through: the Player (macOS Krate.app's open,
/// or the Windows opener, which says so in `KRATE_OPENED_BY`), a window
/// (`--consent`), or a terminal.
pub fn run_door(consent: bool) -> RecordDoor {
    if OPENED_BY_PLAYER.load(std::sync::atomic::Ordering::Relaxed)
        || std::env::var("KRATE_OPENED_BY").as_deref() == Ok("player")
    {
        RecordDoor::Player
    } else if consent {
        RecordDoor::Window
    } else {
        RecordDoor::Terminal
    }
}

/// Where records go, and where they were meant to go when that was not
/// writable.
pub struct Location {
    pub path: PathBuf,
    pub fallback_from: Option<PathBuf>,
}

fn own_path() -> PathBuf {
    crate::krate_home().join("logs").join("opens.jsonl")
}

/// The organisation's default place for the record.
fn system_default() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
            .join("Krate")
            .join("logs")
            .join("opens.jsonl")
    }
    #[cfg(not(windows))]
    {
        let root = if cfg!(debug_assertions) {
            std::env::var(crate::fleet::TEST_POLICY_ROOT_ENV)
                .ok()
                .filter(|r| !r.is_empty())
        } else {
            None
        };
        match root {
            Some(root) => PathBuf::from(root).join("var/log/krate/opens.jsonl"),
            None => PathBuf::from("/var/log/krate/opens.jsonl"),
        }
    }
}

/// Can this process append to `path`, making its folder if it must?
fn writable(path: &Path) -> bool {
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return false;
        }
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .is_ok()
}

/// Where the next record goes.
pub fn location() -> Location {
    let wanted = match crate::fleet::state() {
        crate::fleet::State::None => {
            return Location {
                path: own_path(),
                fallback_from: None,
            }
        }
        crate::fleet::State::InForce { policy, .. } => {
            policy.audit_path.clone().unwrap_or_else(system_default)
        }
        crate::fleet::State::Unreadable { .. } => system_default(),
    };
    if writable(&wanted) {
        Location {
            path: wanted,
            fallback_from: None,
        }
    } else {
        Location {
            path: own_path(),
            fallback_from: Some(wanted),
        }
    }
}

/// Now, as `2026-10-14T09:12:03Z`.
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339(secs)
}

fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rest = secs % 86_400;
    // Howard Hinnant's civil-from-days, as `org::when` does.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

/// The OS login name of whoever is running Krate.
fn user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// sha256 of a file, read in pieces so a large app is not held twice.
pub fn sha256_of_file(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Some(
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}

/// A `refused` record: the app's fields, the stable code and the words.
pub fn refused(base: &Value, refused: &Refused) {
    let mut record = base.clone();
    record["event"] = json!("refused");
    record["reason"] = json!(refused.reason);
    record["words"] = json!(refused.words);
    write(record);
}

/// Append one record. Best effort: a record that cannot be written never
/// stops an app opening, or changes a refusal.
pub fn write(mut record: Value) {
    record["v"] = json!(1);
    record["at"] = json!(now_rfc3339());
    record["user"] = json!(user());
    record["krate"] = json!(crate::KRATE_VERSION_NUMBER);
    let place = location();
    append(&place.path, &record);
}

/// Rotate past the limit, then append the line with one write.
pub fn append(path: &Path, record: &Value) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() > ROTATE_AT) {
        let _ = std::fs::rename(path, path.with_file_name("opens.1.jsonl"));
    }
    let mut line = record.to_string();
    line.push('\n');
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_time_is_rfc3339_in_utc() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_760_433_123), "2025-10-14T09:12:03Z");
    }

    #[test]
    fn past_ten_mebibytes_the_record_rotates_and_keeps_one_generation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("opens.jsonl");
        std::fs::write(&path, vec![b'x'; (ROTATE_AT + 1) as usize]).expect("big file");
        std::fs::write(dir.path().join("opens.1.jsonl"), "old generation\n").expect("old");
        append(&path, &json!({"event": "opened"}));
        let rotated = std::fs::metadata(dir.path().join("opens.1.jsonl")).expect("rotated");
        assert_eq!(rotated.len(), ROTATE_AT + 1, "the full file moved aside");
        let fresh = std::fs::read_to_string(&path).expect("fresh");
        assert_eq!(fresh, "{\"event\":\"opened\"}\n");
        // Under the limit, nothing moves.
        append(&path, &json!({"event": "refused"}));
        assert_eq!(
            std::fs::read_to_string(&path).expect("two").lines().count(),
            2
        );
    }
}
