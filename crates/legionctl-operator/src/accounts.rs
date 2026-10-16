//! Account usage over HTTP (todo-136): one GET per account and agent to the provider's usage endpoint with the
//! profile's own access token (`legionctl_core::accounts`), and the switch of an operator member at its limit. The
//! request headers are those of the agents' own clients, as clauth 0.17.0 sends them (MIT, Copyright (c) 2026
//! cloudy; THIRD-PARTY-NOTICES.md). Config comes from the layers without the repository's: a repository cannot
//! point a token at another host or pick an account.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use legionctl_core::accounts::{self, AccountUsage, AccountsConfig, Login};
use legionctl_core::config::{Config, Dirs, resolve};

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The config of the defaults, /etc/legionctl and the user layer.
///
/// # Errors
/// The config does not resolve.
pub fn config() -> Result<Config> {
    Ok(resolve(&Dirs::from_env(), None)?.config)
}

/// One GET of `agent`'s usage endpoint with `login`'s token, as its agent's client sends it.
async fn fetch(url: &str, agent: &str, login: &Login) -> Result<Vec<accounts::Window>> {
    static HTTP: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    let http = HTTP.get_or_init(reqwest::Client::new);
    let mut request = http
        .get(url)
        .bearer_auth(&login.token)
        .timeout(Duration::from_secs(15));
    request = if agent == "claude" {
        request
            .header("anthropic-beta", "oauth-2025-04-20")
            .header("User-Agent", "claude-cli (external, cli)")
    } else {
        let request = request.header("User-Agent", "codex_cli_rs");
        match &login.account_id {
            Some(id) => request.header("ChatGPT-Account-ID", id),
            None => request,
        }
    };
    // the error names the host, never the request: it holds the token
    let response = request
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("{url} does not answer: {}", e.without_url()))?;
    let status = response.status();
    let headers = response.headers().clone();
    let text = response.text().await.context("reading the answer")?;
    match status.as_u16() {
        401 | 403 => bail!("login needed: {url} refused the access token ({status})"),
        429 => bail!(
            "{url} limits the usage reads (429); retry after {} s",
            headers
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("?")
        ),
        s if s >= 400 => bail!(
            "{url} answered {status}: {}",
            text.chars().take(200).collect::<String>()
        ),
        _ => {}
    }
    let body: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("the answer of {url} is not JSON"))?;
    if agent == "claude" {
        accounts::claude_windows(&body)
    } else {
        accounts::codex_windows(&body)
    }
}

/// The usage of `account`'s `agent`: the kept answer, else (or with `fresh`) a read from the provider, kept with its
/// fetch time; a failed read is the answer's error.
///
/// # Errors
/// The cache fails.
pub fn usage_of(
    runtime: &tokio::runtime::Handle,
    config: &Config,
    (account, agent): (&str, &str),
    fresh: bool,
) -> Result<AccountUsage> {
    if !fresh && let Some(kept) = accounts::cached(account, agent)? {
        return Ok(kept);
    }
    let a = &config.agents.accounts;
    let url = if agent == "claude" {
        &a.endpoints.claude
    } else {
        &a.endpoints.codex
    };
    let now = now_ms();
    let read = accounts::login(account, agent, now)
        .and_then(|login| runtime.block_on(fetch(url, agent, &login)));
    let usage = AccountUsage {
        account: account.into(),
        agent: agent.into(),
        fetched_at: now / 1000,
        windows: read.as_ref().map(Clone::clone).unwrap_or_default(),
        error: read.err().map(|e| format!("{e:#}")),
    };
    let max = config.cache.get("accounts").map_or(0, |c| c.max_bytes);
    accounts::keep(&usage, a.usage_ttl_s, max)?;
    Ok(usage)
}

/// `agents accounts usage`: each logged-in agent of each profile (or of `account`).
///
/// # Errors
/// No such profile, a profile or its keyring cannot be read, or the cache fails.
pub fn usage(
    runtime: &tokio::runtime::Handle,
    input: &legionctl_api::AccountUsageIn,
) -> Result<Vec<AccountUsage>> {
    let config = config()?;
    let names = match &input.account {
        Some(name) => {
            legionctl_core::session::account_dir(name)?;
            vec![name.clone()]
        }
        None => accounts::profiles()?,
    };
    let mut out = Vec::new();
    for name in &names {
        for agent in ["claude", "codex"] {
            if accounts::login_file(name, agent)?.is_some() {
                out.push(usage_of(runtime, &config, (name, agent), input.fresh)?);
            }
        }
    }
    Ok(out)
}

/// What a wake does with its member's account.
pub enum Decision {
    /// It runs on its account (none: the home's logins).
    Stay,
    /// Its account is at its limit: it continues on `to`.
    Move { to: String, why: String },
}

/// What an operator member of `agent` on `account` does at a wake: when its account is at its limit and
/// `agents.accounts.fallback.<agent>` lists accounts, it moves to the first one under the limit. A member whose
/// agent has no list, or that runs on the home's logins, stays.
///
/// # Errors
/// The usage of its account cannot be read, or its account is at its limit and every other listed account is too or
/// cannot be read (the wake fails with this reason), or the config or the cache fails.
pub fn decide(
    runtime: &tokio::runtime::Handle,
    (agent, account): (&str, Option<&str>),
) -> Result<Decision> {
    let config = config()?;
    let AccountsConfig {
        fallback,
        switch_at,
        ..
    } = &config.agents.accounts;
    let (Some(order), Some(account)) = (fallback.get(agent), account) else {
        return Ok(Decision::Stay);
    };
    let now = usage_of(runtime, &config, (account, agent), false)?;
    if let Some(error) = now.error {
        bail!("the usage of account {account} cannot be read: {error}");
    }
    let Some(full) = now.at_limit(*switch_at) else {
        return Ok(Decision::Stay);
    };
    let mut why = Vec::new();
    for next in order.iter().filter(|n| *n != account) {
        let u = usage_of(runtime, &config, (next, agent), false)?;
        match (u.error.clone(), u.at_limit(*switch_at)) {
            (Some(error), _) => why.push(format!("{next}: {error}")),
            (None, Some(limit)) => why.push(format!("{next}: {limit}")),
            (None, None) => {
                return Ok(Decision::Move {
                    to: next.clone(),
                    why: format!("account {account} has {full} (switch at {switch_at:.0}%)"),
                });
            }
        }
    }
    bail!(
        "account {account} has {full} (switch at {switch_at:.0}%) and no account of agents.accounts.fallback.{agent} \
         is under it: {}",
        if why.is_empty() {
            "the list has no other account".to_string()
        } else {
            why.join("; ")
        }
    )
}
