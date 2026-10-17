//! The accounts (todo-136, todo-188): clauth (<https://github.com/uwuclxdy/clauth>, a runtime dependency, 0.17) is the
//! account engine and the only store of the user's logins. legionctl reads clauth's roster and login files (to bind
//! a profile's login into a sandbox and to see two profiles with one login), asks clauth which profile owns the live
//! login (`clauth which`), and takes each account's use, its auth state, its fallback chain and its live sessions from
//! clauth's status feed (`clauth status --json`, schema 2, written by clauth's daemon). Tokens are compared in memory
//! and dropped, never printed.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The clauth versions whose files and status feed legionctl reads.
pub const CLAUTH_SUPPORTED: &str = "0.17";
/// The schema of clauth's status feed that legionctl reads.
const STATUS_SCHEMA: u64 = 2;

/// One rate-limit window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Window {
    /// clauth's label of the window (`5h`, `7d`, ...).
    pub name: String,
    pub used_percent: f64,
    /// When the window resets (seconds since the epoch).
    pub resets_at: Option<i64>,
}

/// One account's use of one agent's provider, from clauth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AccountUsage {
    pub account: String,
    /// Where its login is: `current` or `clauth` (`Held`).
    pub source: String,
    /// The current login, or clauth's active profile.
    pub active: bool,
    pub agent: String,
    pub windows: Vec<Window>,
    /// Why there is no use to show; the windows are empty then.
    pub error: Option<String>,
    /// When clauth read the use (seconds since the epoch).
    pub fetched_at: Option<i64>,
    /// One plain sentence for the user: the agent, its use or why there is none, clauth's state of the account, and
    /// when it was read.
    pub text: String,
    /// The step that gets the use read (Start clauth), as a button.
    pub actions: Vec<Fix>,
}

/// A button that fixes a state: its words and the operation it runs with this input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Fix {
    pub name: String,
    pub op: String,
    pub input: serde_json::Value,
}

/// clauth's status feed (`clauth status --json`, schema 2): the fields legionctl reads.
#[derive(Debug, Clone, Deserialize)]
pub struct Status {
    pub schema: u64,
    pub clauth_version: String,
    pub profiles: Vec<ProfileStatus>,
}

/// One profile in clauth's status feed.
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileStatus {
    pub name: String,
    /// `claude` or `codex`.
    pub harness: String,
    pub active: bool,
    /// A `clauth start` session runs on it.
    pub has_live_session: bool,
    /// `ok`, `expired` (the access token is past its time; clauth renews it at its next use) or `broken` (the login
    /// was refused).
    pub auth_status: String,
    pub fetched_at: Option<String>,
    /// Its place in clauth's fallback chain.
    pub fallback: Option<Chain>,
    pub windows: Vec<StatusWindow>,
}

/// A profile's place in clauth's fallback chain: auto-switch moves on from it at `threshold` percent.
#[derive(Debug, Clone, Deserialize)]
pub struct Chain {
    pub position: usize,
    pub threshold: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StatusWindow {
    pub label: String,
    pub utilization_pct: f64,
    pub resets_at: Option<String>,
}

impl ProfileStatus {
    /// The windows as legionctl shows them.
    #[must_use]
    pub fn windows(&self) -> Vec<Window> {
        self.windows
            .iter()
            .map(|w| Window {
                name: w.label.clone(),
                used_percent: w.utilization_pct,
                resets_at: w.resets_at.as_deref().and_then(iso_seconds),
            })
            .collect()
    }

    /// Why the account is at its chain's limit (`5h at 93%`), when a window is at its threshold or more.
    #[must_use]
    pub fn at_limit(&self) -> Option<String> {
        let chain = self.fallback.as_ref()?;
        self.windows
            .iter()
            .find(|w| w.utilization_pct >= chain.threshold)
            .map(|w| format!("{} at {:.0}%", w.label, w.utilization_pct))
    }

    /// When clauth read the use (seconds since the epoch).
    #[must_use]
    pub fn fetched_at(&self) -> Option<i64> {
        self.fetched_at.as_deref().and_then(iso_seconds)
    }
}

/// The clauth program: on the PATH, else in cargo's folder (`cargo install` puts it there); none when it is not
/// installed.
#[must_use]
pub fn clauth_bin() -> Option<PathBuf> {
    crate::setup::tool_bin("clauth", ".cargo/bin")
}

/// `clauth <args>` with no terminal: its output; none when clauth is not installed.
///
/// # Errors
/// clauth fails; the error has clauth's words.
pub fn clauth(args: &[&str]) -> Result<Option<String>> {
    let Some(bin) = clauth_bin() else {
        return Ok(None);
    };
    let out = std::process::Command::new(bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .context("running clauth")?;
    anyhow::ensure!(
        out.status.success(),
        "clauth {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// clauth's status feed; none when clauth is not installed.
///
/// # Errors
/// clauth fails, or its feed is not the schema legionctl reads.
pub fn status() -> Result<Option<Status>> {
    let Some(text) = clauth(&["status", "--json"])? else {
        return Ok(None);
    };
    let status: Status =
        serde_json::from_str(&text).context("clauth's status feed is not JSON of its schema")?;
    anyhow::ensure!(
        status.schema == STATUS_SCHEMA,
        "clauth {} writes its status in schema {}; legionctl reads schema {STATUS_SCHEMA} (clauth \
         {CLAUTH_SUPPORTED})",
        status.clauth_version,
        status.schema
    );
    Ok(Some(status))
}

/// clauth's daemon runs (it reads the use and switches at the limit).
#[must_use]
pub fn daemon_runs() -> bool {
    clauth(&["daemon", "--status"]).is_ok_and(|o| o.is_some())
}

/// One agent's login of an account, and where it is. clauth is the only store of the user's logins (user decision
/// 2026-10-17):
/// - `current`: the login Claude Code and Codex use now (`~/.claude/.credentials.json`, `~/.codex/auth.json`), named
///   by the clauth profile that holds it (as `clauth which` does: the same refresh token), else `current`;
/// - `clauth`: another profile of clauth (`~/.clauth/profiles/<name>`, read only; `active`: clauth's active one).
pub struct Held {
    pub name: String,
    pub source: &'static str,
    pub agent: String,
    pub active: bool,
    /// The clauth profile that holds the same login (the same refresh token): a refresh through one spends the
    /// other's token.
    pub same_as: Option<String>,
}

/// A file's bytes; none when it is not there.
fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn home() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os("HOME").context("HOME is not set")?,
    ))
}

/// The login file that `agent` uses now.
///
/// # Errors
/// HOME is not set.
pub fn live_file(agent: &str) -> Result<PathBuf> {
    Ok(if agent == "claude" {
        home()?.join(".claude/.credentials.json")
    } else {
        home()?.join(".codex/auth.json")
    })
}

/// The login file of `agent` in clauth profile `name` (clauth 0.17.0 `profile_dir`: `credentials.json`,
/// `auth.json`).
fn clauth_file(name: &str, agent: &str) -> Result<PathBuf> {
    Ok(home()?
        .join(".clauth/profiles")
        .join(name)
        .join(if agent == "claude" {
            "credentials.json"
        } else {
            "auth.json"
        }))
}

/// clauth's roster of `agent` (clauth 0.17.0: `profiles.toml` for Claude Code, `codex-profiles.toml` for Codex): its
/// active profile and its profiles; none when clauth has no roster.
///
/// # Errors
/// The roster cannot be read or is not TOML with a list `profiles`.
pub fn clauth_roster(agent: &str) -> Result<(Option<String>, Vec<String>)> {
    let path = home()?.join(".clauth").join(if agent == "claude" {
        "profiles.toml"
    } else {
        "codex-profiles.toml"
    });
    let text = match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((None, Vec::new())),
        text => text.with_context(|| format!("reading {}", path.display()))?,
    };
    let roster: toml::Table =
        toml::from_str(&text).with_context(|| format!("{} is not TOML", path.display()))?;
    let names = match roster.get("profiles") {
        None => Vec::new(),
        Some(list) => list
            .as_array()
            .and_then(|l| l.iter().map(|n| n.as_str().map(String::from)).collect())
            .with_context(|| format!("{}: profiles is not a list of names", path.display()))?,
    };
    let active = roster
        .get("active_profile")
        .and_then(|a| a.as_str())
        .map(String::from);
    Ok((active, names))
}

/// The name of `agent` for the user.
#[must_use]
pub fn named(agent: &str) -> &'static str {
    if agent == "claude" {
        "Claude Code"
    } else {
        "Codex"
    }
}

/// The login file of `agent` in clauth account `name`, for a session that runs on it.
///
/// # Errors
/// clauth has no such account, or it has no login of `agent`.
pub fn profile_login(name: &str, agent: &str) -> Result<PathBuf> {
    exists(name)?;
    let file = clauth_file(name, agent)?;
    anyhow::ensure!(
        clauth_roster(agent)?.1.iter().any(|n| n == name) && file.is_file(),
        "account {name} has no {} login",
        named(agent)
    );
    Ok(file)
}

/// clauth has account `name` (of Claude Code or Codex).
///
/// # Errors
/// It has not; the error lists the accounts it has.
pub fn exists(name: &str) -> Result<()> {
    let mut all = Vec::new();
    for agent in ["claude", "codex"] {
        for n in clauth_roster(agent)?.1 {
            if !all.contains(&n) {
                all.push(n);
            }
        }
    }
    if all.iter().any(|n| n == name) {
        return Ok(());
    }
    if all.is_empty() {
        bail!("no account {name}: clauth has no accounts yet; log in to one first");
    }
    bail!("no account {name}; the accounts are: {}", all.join(", "))
}

/// The refresh token of a login file of `agent`, compared and dropped, never kept.
fn refresh_token(agent: &str, bytes: &[u8]) -> Option<String> {
    let file: Value = serde_json::from_slice(bytes).ok()?;
    let token = if agent == "claude" {
        &file["claudeAiOauth"]["refreshToken"]
    } else {
        &file["tokens"]["refresh_token"]
    };
    token.as_str().filter(|t| !t.is_empty()).map(String::from)
}

/// Every login: the current ones and clauth's other profiles (`Held`); with no clauth installed, the current ones.
///
/// # Errors
/// HOME is not set, a login file or clauth's roster cannot be read, or `clauth which` fails.
pub fn logins() -> Result<Vec<Held>> {
    let mut out = Vec::new();
    let installed = clauth_bin().is_some();
    // the owner of Claude Code's live login, as clauth names it (`unknown`: none)
    let which = if installed {
        clauth(&["which"])?
            .map(|w| w.trim().to_string())
            .filter(|w| w != "unknown")
    } else {
        None
    };
    for agent in ["claude", "codex"] {
        let (active, names) = if installed {
            clauth_roster(agent)?
        } else {
            (None, Vec::new())
        };
        let live = live_file(agent)?;
        let live_token = read(&live)?.map(|b| refresh_token(agent, &b));
        let mut held = Vec::new();
        for name in &names {
            let file = clauth_file(name, agent)?;
            let Some(bytes) = read(&file)? else { continue };
            let token = refresh_token(agent, &bytes);
            held.push((
                Held {
                    name: name.clone(),
                    source: "clauth",
                    agent: agent.into(),
                    active: active.as_ref() == Some(name),
                    same_as: None,
                },
                token,
            ));
        }
        // the current login's owner: Claude Code's as `clauth which` names it; Codex's (clauth has no which for it)
        // the profile with the same refresh token, the active one first; else, when Claude Code replaced clauth's
        // link with a plain file (its refresh writes a new file), the active profile, as clauth's own switch sees it
        // (`live_diverged_and_unsaved`): the live file is its newer login
        let current = live_token.as_ref().and_then(Option::as_ref);
        let same = |t: &Option<String>| current.is_some() && t.as_ref() == current;
        let diverged = current.is_some()
            && live
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_file());
        let owner = if agent == "claude" {
            which
                .as_ref()
                .and_then(|w| held.iter().position(|(h, _)| h.name == *w))
        } else {
            held.iter()
                .position(|(h, t)| same(t) && h.active)
                .or_else(|| held.iter().position(|(_, t)| same(t)))
        }
        .or_else(|| held.iter().position(|(h, _)| h.active).filter(|_| diverged));
        // of the profiles with one login, the owner (else the first) holds it; each other one names it
        let first: Vec<Option<String>> = held
            .iter()
            .enumerate()
            .map(|(i, (_, t))| {
                let t = t.as_ref()?;
                let holder = owner
                    .filter(|o| held[*o].1.as_ref() == Some(t))
                    .or_else(|| held.iter().position(|(_, u)| u.as_ref() == Some(t)))?;
                (holder != i).then(|| held[holder].0.name.clone())
            })
            .collect();
        for ((h, _), same) in held.iter_mut().zip(first) {
            h.same_as = same;
        }
        if live_token.is_some() {
            let name = owner.map_or_else(|| "current".to_string(), |o| held[o].0.name.clone());
            out.push(Held {
                name,
                source: "current",
                agent: agent.into(),
                active: true,
                same_as: None,
            });
        }
        out.extend(
            held.into_iter()
                .enumerate()
                .filter(|(i, _)| Some(*i) != owner)
                .map(|(_, (h, _))| h),
        );
    }
    Ok(out)
}

/// legionctl's own account profiles from before clauth kept the logins (`~/.local/share/legionctl/accounts/<name>`):
/// the folder and the names in it, when it is there. Nothing reads them now; doctor says what to do.
#[must_use]
pub fn old_profiles() -> Option<(PathBuf, Vec<String>)> {
    let root = crate::packages::Store::default_root().with_file_name("accounts");
    let mut names: Vec<String> = std::fs::read_dir(&root)
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Some((root, names))
}

/// Seconds since the epoch of an ISO 8601 time (`2026-10-17T05:00:00.123+00:00`, `...Z`), ported from clauth 0.17.0
/// `iso_to_epoch_secs` (MIT License, Copyright (c) 2026 cloudy; THIRD-PARTY-NOTICES.md).
#[must_use]
pub fn iso_seconds(text: &str) -> Option<i64> {
    let num = |r: std::ops::Range<usize>| text.get(r)?.parse::<i64>().ok();
    let bytes = text.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let clock = num(11..13)? * 3_600 + num(14..16)? * 60 + num(17..19)?;
    let tail = &text[19..];
    let zone = match tail.strip_prefix('.') {
        Some(rest) => rest.trim_start_matches(|c: char| c.is_ascii_digit()),
        None => tail,
    };
    let offset = if zone.is_empty() || zone == "Z" {
        0
    } else {
        let sign = match zone.as_bytes()[0] {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        let digits: String = zone[1..].chars().filter(|c| *c != ':').collect();
        let (hours, minutes) = match digits.len() {
            2 => (digits.parse::<i64>().ok()?, 0),
            4 => (
                digits[..2].parse::<i64>().ok()?,
                digits[2..].parse::<i64>().ok()?,
            ),
            _ => return None,
        };
        sign * (hours * 3_600 + minutes * 60)
    };
    // days from the civil date (Howard Hinnant)
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = if shifted >= 0 { shifted } else { shifted - 399 } / 400;
    let of_era = shifted - era * 400;
    let of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let days = era * 146_097 + of_era * 365 + of_era / 4 - of_era / 100 + of_year - 719_468;
    Some(days * 86_400 + clock - offset)
}
