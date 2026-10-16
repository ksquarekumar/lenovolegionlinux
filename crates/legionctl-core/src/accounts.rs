//! Account usage (todo-136): the rate-limit windows (Claude: 5 h and 7 d; Codex: its primary and secondary windows)
//! of each account profile's Claude Code and Codex login, as the provider reports them, read with the profile's own
//! access token. The token is only read: never refreshed (both agents rotate their refresh tokens, DESIGN-12 wall
//! 1), never written, printed or logged. A read that fails is that account's error with its cause, never zero use.
//! Each answer, or its error, is kept in the cache (namespace `accounts`) with its fetch time.
//!
//! The endpoints, their request headers and the shapes of their answers are ported from clauth 0.17.0
//! (<https://github.com/uwuclxdy/clauth>: `src/usage/fetch.rs`, `src/usage/codex.rs`, `src/usage/codex_headers.rs`),
//! MIT License, Copyright (c) 2026 cloudy (THIRD-PARTY-NOTICES.md). Both endpoints are undocumented: a changed
//! answer is an error that names the field.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::session::{LOGINS, accounts_root, keyring_get};

/// `agents.accounts` (only the defaults, /etc/legionctl and the user layer apply).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountsConfig {
    /// How long an answer (or its error) is kept.
    pub usage_ttl_s: u64,
    /// A window at this percent or more is at its limit.
    pub switch_at: f64,
    /// Agent (claude, codex) -> the account profiles that its operator members switch to, in order.
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

/// The account profiles, by name.
///
/// # Errors
/// The profiles folder cannot be read.
pub fn profiles() -> Result<Vec<String>> {
    let root = accounts_root();
    let entries = match std::fs::read_dir(&root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        entries => entries.with_context(|| format!("listing {}", root.display()))?,
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            out.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    out.sort();
    Ok(out)
}

/// The login file of `agent` in profile `account`: in the profile while a session of the account runs, else at rest
/// in the keyring (`legionctl-login <account>/<agent>`, design 12 A1); none when the agent never logged in there.
///
/// # Errors
/// The agent is not claude or codex, the file cannot be read, or secret-tool does not run.
pub fn login_file(account: &str, agent: &str) -> Result<Option<Vec<u8>>> {
    let (_, file) = LOGINS
        .iter()
        .find(|(a, _)| *a == agent)
        .with_context(|| format!("{agent} has no login file: use claude or codex"))?;
    let path = accounts_root().join(account).join(agent).join(file);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            keyring_get(&format!("{account}/{agent}"))
        }
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The access token of `agent`'s login in profile `account`, unchanged: an expired Claude token is an error (the
/// agent refreshes it at its next start), never refreshed here.
///
/// # Errors
/// Not logged in, the login file does not parse or has no access token, or the token expired.
pub fn login(account: &str, agent: &str, now_ms: i64) -> Result<Login> {
    let Some(bytes) = login_file(account, agent)? else {
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
                "login needed: the access token of account {account} expired at {}; start Claude Code on the \
                 account once (it refreshes its own token)",
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

/// The kept answer of `account`/`agent` while it lives.
///
/// # Errors
/// The cache cannot be opened or read, or the kept value does not parse.
pub fn cached(account: &str, agent: &str) -> Result<Option<AccountUsage>> {
    crate::cache::Cache::open()?
        .get(NS, &format!("{account}/{agent}"))?
        .map(|e| serde_json::from_slice(&e.value).context("a kept account usage"))
        .transpose()
}

/// Keep `usage` for `ttl_s` seconds within `max_bytes` (`cache.accounts.max_bytes`).
///
/// # Errors
/// The cache cannot be opened or written.
pub fn keep(usage: &AccountUsage, ttl_s: u64, max_bytes: u64) -> Result<()> {
    crate::cache::Cache::open()?.put(
        (NS, &format!("{}/{}", usage.account, usage.agent)),
        &crate::cache::Entry {
            value: serde_json::to_vec(usage)?,
            meta: Value::Null,
        },
        ttl_s,
        max_bytes,
    )
}
