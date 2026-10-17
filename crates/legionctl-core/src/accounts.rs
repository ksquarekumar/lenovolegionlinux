//! Account usage (todo-136): the rate-limit windows (Claude: 5 h and 7 d; Codex: its primary and secondary windows)
//! of each Claude Code and Codex login (the current one and clauth's profiles: `Held`),
//! as the provider reports them, read with the login's own access token. The token is only read: never refreshed
//! (both agents rotate their refresh tokens, DESIGN-12 wall 1), never written, printed or logged. A read that fails is that account's error with its cause, never zero use.
//! Each answer, or its error, is kept in the cache (namespace `accounts`) with its fetch time.
//!
//! The endpoints, their request headers and the shapes of their answers are ported from clauth 0.17.0
//! (<https://github.com/uwuclxdy/clauth>: `src/usage/fetch.rs`, `src/usage/codex.rs`, `src/usage/codex_headers.rs`),
//! MIT License, Copyright (c) 2026 cloudy (THIRD-PARTY-NOTICES.md). Both endpoints are undocumented: a changed
//! answer is an error that names the field.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `agents.accounts` (only the defaults, /etc/legionctl and the user layer apply).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountsConfig {
    /// How long an answer (or its error) is kept.
    pub usage_ttl_s: u64,
    /// A window at this percent or more is at its limit.
    pub switch_at: f64,
    /// Agent (claude, codex) -> the clauth accounts that its operator members switch to, in order.
    pub fallback: BTreeMap<String, Vec<String>>,
    pub endpoints: Endpoints,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Endpoints {
    pub claude: String,
    pub codex: String,
}

/// One rate-limit window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Window {
    /// `5h`, `7d`, or the window's length.
    pub name: String,
    pub used_percent: f64,
    /// When the window resets (seconds since the epoch).
    pub resets_at: Option<i64>,
}

/// One account's use of one agent's provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AccountUsage {
    pub account: String,
    /// Where its login is: `current` or `clauth` (`Held`).
    pub source: String,
    /// The current login, or clauth's active profile.
    pub active: bool,
    pub agent: String,
    pub windows: Vec<Window>,
    /// Why the read failed; the windows are empty then.
    pub error: Option<String>,
    /// When the provider answered (or the read failed).
    pub fetched_at: i64,
}

impl AccountUsage {
    /// Why the account is at its limit (`5h at 93%`), when a window is at `switch_at` percent or more.
    #[must_use]
    pub fn at_limit(&self, switch_at: f64) -> Option<String> {
        self.windows
            .iter()
            .find(|w| w.used_percent >= switch_at)
            .map(|w| format!("{} at {:.0}%", w.name, w.used_percent))
    }
}

/// An access token. No `Debug` and no `Serialize`: it never reaches a log or an answer.
pub struct Login {
    pub token: String,
    /// Codex's `ChatGPT` account (the `ChatGPT-Account-ID` header).
    pub account_id: Option<String>,
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
    file: PathBuf,
}

impl Held {
    fn bytes(&self) -> Result<Option<Vec<u8>>> {
        read(&self.file)
    }
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
fn named(agent: &str) -> &'static str {
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

/// Every login: the current ones and clauth's other profiles (`Held`).
///
/// # Errors
/// HOME is not set, or a login file or clauth's roster cannot be read.
pub fn logins() -> Result<Vec<Held>> {
    let mut out = Vec::new();
    for agent in ["claude", "codex"] {
        let (active, names) = clauth_roster(agent)?;
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
                    file,
                },
                token,
            ));
        }
        // the current login's owner: the profile that clauth's link points to, else the first with the same refresh
        // token
        let current = live_token.as_ref().and_then(Option::as_ref);
        let linked = live.canonicalize().ok();
        let owner = held
            .iter()
            .position(|(h, t)| {
                t.as_ref() == current && current.is_some() && h.file.canonicalize().ok() == linked
            })
            .or_else(|| {
                held.iter()
                    .position(|(_, t)| t.as_ref() == current && current.is_some())
            });
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
                file: live,
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

/// The login of `agent` of account `name` (`logins`).
///
/// # Errors
/// No login of `agent` has that name, or the logins cannot be read.
pub fn held(name: &str, agent: &str) -> Result<Held> {
    logins()?
        .into_iter()
        .find(|h| h.name == name && h.agent == agent)
        .with_context(|| format!("account {name} has no {} login", named(agent)))
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

/// The access token of `held`, unchanged: an expired Claude token is an error (the agent, or clauth, refreshes it at
/// its next use), never refreshed here.
///
/// # Errors
/// Not logged in, the login file does not parse or has no access token, or the token expired.
pub fn login(held: &Held, now_ms: i64) -> Result<Login> {
    let (account, agent) = (&held.name, held.agent.as_str());
    let Some(bytes) = held.bytes()? else {
        bail!("login needed: {agent} is not logged in on account {account}");
    };
    let file: Value = serde_json::from_slice(&bytes).map_err(|e| {
        anyhow::anyhow!("the {agent} login file of account {account} is not JSON: {e}")
    })?;
    let text = |v: &Value| v.as_str().filter(|t| !t.is_empty()).map(String::from);
    if agent == "claude" {
        let oauth = &file["claudeAiOauth"];
        let token = text(&oauth["accessToken"])
            .context("the claude login file has no claudeAiOauth.accessToken")?;
        if let Some(expires) = oauth["expiresAt"].as_i64().filter(|e| *e <= now_ms) {
            bail!(
                "login needed: the access token of account {account} expired at {}; Claude Code refreshes it when \
                 it next runs on the account",
                expires / 1000
            );
        }
        return Ok(Login {
            token,
            account_id: None,
        });
    }
    let tokens = &file["tokens"];
    let token = text(&tokens["access_token"]).context(
        "the codex login file has no tokens.access_token (an API key login has no ChatGPT usage)",
    )?;
    Ok(Login {
        token,
        account_id: text(&tokens["account_id"]),
    })
}

/// A number field of `v`, or the error that names it.
fn percent(v: &Value, field: &str) -> Result<f64> {
    v[field]
        .as_f64()
        .with_context(|| format!("the answer has no number {field}"))
}

/// Claude's windows from `/api/oauth/usage` (clauth `windows_from_raw`): `limits[]`, where `session` is 5h and
/// `weekly_all` 7d. No other field is read.
///
/// # Errors
/// The answer has no `limits` list, no `session` or `weekly_all` entry, or its `percent` is not a number.
pub fn claude_windows(body: &Value) -> Result<Vec<Window>> {
    let limits = body["limits"]
        .as_array()
        .context("the answer has no list limits")?;
    [("5h", "session"), ("7d", "weekly_all")]
        .into_iter()
        .map(|(name, kind)| {
            let i = limits
                .iter()
                .position(|l| l["kind"] == kind)
                .with_context(|| format!("the answer has no limits[] entry of kind {kind}"))?;
            Ok(Window {
                name: name.into(),
                used_percent: percent(&limits[i], "percent")
                    .with_context(|| format!("limits[{i}] ({kind})"))?,
                resets_at: limits[i]["resets_at"].as_str().and_then(iso_seconds),
            })
        })
        .collect()
}

/// A window's name from its length: `5h`, `7d`, `90m`.
fn length(seconds: i64) -> String {
    match seconds {
        s if s % 86_400 == 0 => format!("{}d", s / 86_400),
        s if s % 3_600 == 0 => format!("{}h", s / 3_600),
        s => format!("{}m", s / 60),
    }
}

/// Codex's windows from `/backend-api/wham/usage` (clauth `map_usage`): `rate_limit.primary_window` and
/// `secondary_window` (null when the plan has none), named by `limit_window_seconds`; when `limit_reached`, the
/// fuller one counts as 100%.
///
/// # Errors
/// The answer has no object `rate_limit`, no window, or a window without a number `used_percent` or
/// `limit_window_seconds`.
pub fn codex_windows(body: &Value) -> Result<Vec<Window>> {
    let limit = &body["rate_limit"];
    anyhow::ensure!(limit.is_object(), "the answer has no object rate_limit");
    let mut out = Vec::new();
    for slot in ["primary_window", "secondary_window"] {
        let w = &limit[slot];
        if w.is_null() {
            continue;
        }
        let seconds = w["limit_window_seconds"].as_i64().with_context(|| {
            format!("rate_limit.{slot}: the answer has no number limit_window_seconds")
        })?;
        out.push(Window {
            name: length(seconds),
            used_percent: percent(w, "used_percent")
                .with_context(|| format!("rate_limit.{slot}"))?,
            resets_at: w["reset_at"].as_i64().filter(|at| *at > 0),
        });
    }
    if limit["limit_reached"] == true
        && let Some(full) = out
            .iter_mut()
            .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
    {
        full.used_percent = full.used_percent.max(100.0);
    }
    anyhow::ensure!(!out.is_empty(), "the answer has no rate_limit window");
    Ok(out)
}

/// Seconds since the epoch of an ISO 8601 time (`2026-10-17T05:00:00.123+00:00`, `...Z`) (clauth
/// `iso_to_epoch_secs`).
fn iso_seconds(text: &str) -> Option<i64> {
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

const NS: &str = "accounts";

/// The kept answer of `held` while it lives.
///
/// # Errors
/// The cache cannot be opened or read, or the kept value does not parse.
pub fn cached(held: &Held) -> Result<Option<AccountUsage>> {
    crate::cache::Cache::open()?
        .get(NS, &format!("{}/{}/{}", held.source, held.name, held.agent))?
        .map(|e| serde_json::from_slice(&e.value).context("a kept account usage"))
        .transpose()
}

/// Keep `usage` for `ttl_s` seconds within `max_bytes` (`cache.accounts.max_bytes`).
///
/// # Errors
/// The cache cannot be opened or written.
pub fn keep(usage: &AccountUsage, ttl_s: u64, max_bytes: u64) -> Result<()> {
    crate::cache::Cache::open()?.put(
        (
            NS,
            &format!("{}/{}/{}", usage.source, usage.account, usage.agent),
        ),
        &crate::cache::Entry {
            value: serde_json::to_vec(usage)?,
            meta: Value::Null,
        },
        ttl_s,
        max_bytes,
    )
}
