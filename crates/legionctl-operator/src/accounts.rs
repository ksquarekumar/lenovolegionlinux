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
        401 | 403 => bail!(
            "login needed: the provider refused the login ({status}); log in to the account again"
        ),
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

/// The usage of `held`: the kept answer, else (or with `fresh`) a read from the provider, kept with its fetch time; a
/// failed read is the answer's error.
///
/// # Errors
/// The cache fails.
pub fn usage_of(
    runtime: &tokio::runtime::Handle,
    config: &Config,
    held: &accounts::Held,
    fresh: bool,
) -> Result<AccountUsage> {
    let now = now_ms();
    if let Some(other) = &held.same_as {
        // one refresh token in two profiles: no read, the user fixes it in clauth
        return Ok(AccountUsage {
            account: held.name.clone(),
            source: held.source.into(),
            active: held.active,
            agent: held.agent.clone(),
            fetched_at: now / 1000,
            windows: Vec::new(),
            error: Some(format!(
                "{} holds the same login as {other}: a refresh through one logs the other out. Log in to {} again \
                 in clauth, or delete one of the two",
                held.name, held.name
            )),
            text: String::new(),
        });
    }
    if !fresh && let Some(kept) = accounts::cached(held)? {
        return Ok(kept);
    }
    let a = &config.agents.accounts;
    let url = if held.agent == "claude" {
        &a.endpoints.claude
    } else {
        &a.endpoints.codex
    };
    let read = accounts::login(held, now)
        .and_then(|login| runtime.block_on(fetch(url, &held.agent, &login)));
    let usage = AccountUsage {
        account: held.name.clone(),
        source: held.source.into(),
        active: held.active,
        agent: held.agent.clone(),
        fetched_at: now / 1000,
        windows: read.as_ref().map(Clone::clone).unwrap_or_default(),
        error: read.err().map(|e| format!("{e:#}")),
        text: String::new(),
    };
    let max = config.cache.get("accounts").map_or(0, |c| c.max_bytes);
    accounts::keep(&usage, a.usage_ttl_s, max)?;
    Ok(usage)
}

/// `agents accounts usage`: each login (`accounts::logins`), or those of `account`.
///
/// # Errors
/// No login has that name, a login cannot be read, or the cache fails.
pub fn usage(
    runtime: &tokio::runtime::Handle,
    input: &legionctl_api::AccountUsageIn,
) -> Result<Vec<AccountUsage>> {
    let config = config()?;
    let held: Vec<accounts::Held> = accounts::logins()?
        .into_iter()
        .filter(|h| input.account.as_ref().is_none_or(|a| *a == h.name))
        .collect();
    if let Some(name) = &input.account {
        anyhow::ensure!(!held.is_empty(), "no account {name} has a login");
    }
    let now = now_ms() / 1000;
    held.iter()
        .map(|h| {
            let mut u = usage_of(runtime, &config, h, input.fresh)?;
            u.text = sentence(&u, h.same_as.is_some(), now);
            Ok(u)
        })
        .collect()
}

/// The row's one sentence: `Claude Code, the current login: 5h 21% used, 7d 27% used. Read 4 min ago.`
fn sentence(u: &AccountUsage, same: bool, now: i64) -> String {
    let agent = accounts::named(&u.agent);
    let who = if u.source == "current" {
        format!("{agent}, the current login")
    } else {
        agent.to_string()
    };
    let ago = match now - u.fetched_at {
        s if s < 60 => "just now".to_string(),
        s if s < 3600 => format!("{} min ago", s / 60),
        s if s < 86_400 => format!("{} h ago", s / 3600),
        s => format!("{} d ago", s / 86_400),
    };
    match &u.error {
        Some(e) if same => format!("{who}: {e}."),
        Some(e) => match e.strip_prefix("login needed: ") {
            Some(why) => format!("{who}: {why}. Tried {ago}."),
            None => format!("{who}: the use cannot be read: {e}. Tried {ago}."),
        },
        None => format!(
            "{who}: {}. Read {ago}.",
            u.windows
                .iter()
                .map(|w| format!("{} {:.0}% used", w.name, w.used_percent))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
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
    let now = usage_of(runtime, &config, &accounts::held(account, agent)?, false)?;
    if let Some(error) = now.error {
        bail!("the usage of account {account} cannot be read: {error}");
    }
    let Some(full) = now.at_limit(*switch_at) else {
        return Ok(Decision::Stay);
    };
    let mut why = Vec::new();
    for next in order.iter().filter(|n| *n != account) {
        let u = match accounts::held(next, agent) {
            Ok(held) => usage_of(runtime, &config, &held, false)?,
            Err(e) => {
                why.push(format!("{next}: {e:#}"));
                continue;
            }
        };
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

/// `agents accounts capture`: keep the current logins as account `name`, never as a copy (`AgentsAccountsCapture`).
///
/// # Errors
/// The name is not an account name, a login cannot be read, clauth does not run or refuses, or no login is kept.
pub fn capture(name: &str) -> Result<legionctl_api::Lines> {
    anyhow::ensure!(
        legionctl_core::dossier::valid_name(name),
        "an account name is a-z, 0-9 and '-', starting with a letter or digit"
    );
    let logins = accounts::logins()?;
    let current = |agent: &str| {
        logins
            .iter()
            .find(|h| h.source == "current" && h.agent == agent)
    };
    let mut lines = Vec::new();
    let mut kept = 0;
    match current("codex") {
        None => lines.push("Codex is not logged in".to_string()),
        Some(h) if h.name != "current" => {
            kept += 1;
            lines.push(format!("Codex: the current login is clauth account {} already", h.name));
        }
        Some(_) => lines.push(
            "Codex: the current login is not kept: no clauth account holds it, and a copy of it logs one of the two \
             out (its refresh token rotates); log in to Codex as a new account instead"
                .to_string(),
        ),
    }
    match current("claude") {
        None => lines.push("Claude Code is not logged in".into()),
        Some(h) if h.name != "current" => {
            kept += 1;
            lines.push(format!(
                "Claude Code: the current login is clauth account {} already",
                h.name
            ));
        }
        Some(_) => {
            clauth(&["capture", name])?;
            kept += 1;
            lines.push(format!(
                "Claude Code: clauth keeps the current login as account {name}"
            ));
        }
    }
    anyhow::ensure!(kept > 0, "no login is kept: {}", lines.join("; "));
    Ok(legionctl_api::Lines { lines })
}

/// Run clauth with `args`, with no terminal; its error has clauth's words.
fn clauth(args: &[&str]) -> Result<()> {
    let out = std::process::Command::new("clauth")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| anyhow::anyhow!("clauth keeps the logins, and it does not run: {e}"))?;
    anyhow::ensure!(
        out.status.success(),
        "clauth {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

/// `agents accounts login`: clauth's own login of account `name` (`AgentsAccountsLogin`); it ends when the user has
/// logged in on the provider's page, which clauth opens in the browser.
///
/// # Errors
/// The name is not an account name, clauth does not run, or the login fails.
pub fn login(input: &legionctl_api::AccountLoginIn) -> Result<legionctl_api::Lines> {
    let name = input.name.as_str();
    anyhow::ensure!(
        legionctl_core::dossier::valid_name(name),
        "an account name is a-z, 0-9 and '-', starting with a letter or digit"
    );
    // --browser: a new Codex login of clauth's own, not a copy of the current one (two holders of one token)
    let args: &[&str] = if input.codex {
        &["login", name, "--codex", "--browser"]
    } else {
        &["login", name]
    };
    clauth(args)?;
    Ok(legionctl_api::Lines {
        lines: vec![format!(
            "clauth keeps the {} login of account {name}",
            if input.codex { "Codex" } else { "Claude Code" }
        )],
    })
}

/// `agents accounts switch`: clauth account `name` becomes the current login (`AgentsAccountsSwitch`).
///
/// # Errors
/// clauth has no such account, does not run, or refuses.
pub fn switch(name: &str) -> Result<legionctl_api::Lines> {
    accounts::exists(name)?;
    clauth(&["switch", name])?;
    Ok(legionctl_api::Lines {
        lines: vec![format!("account {name} is the current login")],
    })
}
