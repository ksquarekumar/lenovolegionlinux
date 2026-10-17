//! The accounts over clauth (todo-136, todo-188): each login's use, auth state, fallback chain and sessions from
//! clauth's status feed (its daemon reads the use), the switch of an operator member at its limit along clauth's
//! fallback chain, and clauth's own login, switch and capture. legionctl reads no provider itself.

use std::fmt::Write as _;

use anyhow::{Result, bail};
use legionctl_core::accounts::{self, AccountUsage, Fix, Held, ProfileStatus};
use legionctl_core::setup;

/// `clauth <args>`; clauth must be installed.
fn clauth(args: &[&str]) -> Result<String> {
    accounts::clauth(args)?
        .ok_or_else(|| anyhow::anyhow!("clauth is not installed: Set up installs it"))
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// `agents accounts usage`: each login (`accounts::logins`), or those of `account`, with clauth's state of it.
///
/// # Errors
/// No login has that name, a login cannot be read, or clauth fails or writes another status schema.
pub fn usage(input: &legionctl_api::AccountUsageIn) -> Result<Vec<AccountUsage>> {
    let held: Vec<Held> = accounts::logins()?
        .into_iter()
        .filter(|h| input.account.as_ref().is_none_or(|a| *a == h.name))
        .collect();
    if let Some(name) = &input.account {
        anyhow::ensure!(!held.is_empty(), "no account {name} has a login");
    }
    let status = accounts::status()?;
    // asked once, and only when a use is missing
    let mut daemon = None;
    let now = now();
    Ok(held
        .iter()
        .map(|h| {
            let entry = status.as_ref().and_then(|s| {
                s.profiles
                    .iter()
                    .find(|p| p.name == h.name && p.harness == h.agent)
            });
            let (windows, error, actions) = state(h, status.is_some(), entry, || {
                *daemon.get_or_insert_with(accounts::daemon_runs)
            });
            let mut u = AccountUsage {
                account: h.name.clone(),
                source: h.source.into(),
                active: h.active,
                agent: h.agent.clone(),
                windows,
                error,
                fetched_at: entry.and_then(ProfileStatus::fetched_at),
                text: String::new(),
                actions,
            };
            u.text = sentence(&u, entry, now);
            u
        })
        .collect())
}

/// The windows of `h`, or why there are none and the step that gets them read.
fn state(
    h: &Held,
    installed: bool,
    entry: Option<&ProfileStatus>,
    daemon: impl FnOnce() -> bool,
) -> (Vec<accounts::Window>, Option<String>, Vec<Fix>) {
    let fix = |name: &str, op: &str, input: serde_json::Value| Fix {
        name: name.into(),
        op: op.into(),
        input,
    };
    let why = if let Some(other) = &h.same_as {
        format!(
            "{} holds the same login as {other}: a refresh through one logs the other out. Log in to {} again in \
             clauth, or delete one of the two",
            h.name, h.name
        )
    } else if !installed {
        return (
            Vec::new(),
            Some(UNKNOWN.into()),
            vec![fix(
                "Install clauth",
                "setup.apply",
                serde_json::json!({"install_ctx": false}),
            )],
        );
    } else if h.name == "current" {
        "no clauth account holds this login, so clauth does not read its use".into()
    } else if let Some(p) = entry {
        if p.auth_status == "broken" {
            format!(
                "the provider refused the login of {}; log in to it again",
                h.name
            )
        } else if !p.windows.is_empty() {
            return (p.windows(), None, Vec::new());
        } else if !daemon() {
            return (
                Vec::new(),
                Some(UNKNOWN.into()),
                vec![fix(
                    "Start clauth",
                    "setup.turn-on",
                    serde_json::json!({"service": setup::CLAUTH_DAEMON}),
                )],
            );
        } else if p.auth_status == "expired" {
            "the login has expired; clauth renews it at its next use".into()
        } else {
            "clauth has not read the use yet".into()
        }
    } else {
        format!(
            "clauth does not show {} (it is turned off in clauth)",
            h.name
        )
    };
    (Vec::new(), Some(why), Vec::new())
}

/// The use is not read, and a button gets it read.
const UNKNOWN: &str = "use unknown";

/// The row's one sentence: `Claude Code, the current login: 5h 21% used, 7d 27% used. In clauth's fallback chain at
/// 2: it moves on at 90%. Read 4 min ago.`
fn sentence(u: &AccountUsage, entry: Option<&ProfileStatus>, now: i64) -> String {
    let agent = accounts::named(&u.agent);
    let who = if u.source == "current" {
        format!("{agent}, the current login")
    } else {
        agent.to_string()
    };
    let Some(e) = entry.filter(|_| u.error.is_none()) else {
        return format!("{who}: {}.", u.error.as_deref().unwrap_or_default());
    };
    let mut text = format!(
        "{who}: {}.",
        u.windows
            .iter()
            .map(|w| format!("{} {:.0}% used", w.name, w.used_percent))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if e.auth_status == "expired" {
        text.push_str(" The login has expired; clauth renews it at its next use.");
    }
    if let Some(chain) = &e.fallback {
        let _ = write!(
            text,
            " In clauth's fallback chain at {}: it moves on at {:.0}%.",
            chain.position, chain.threshold
        );
    }
    if e.has_live_session {
        text.push_str(" A clauth session runs on it.");
    }
    if let Some(at) = u.fetched_at {
        let _ = match now - at {
            s if s < 60 => write!(text, " Read just now."),
            s if s < 3600 => write!(text, " Read {} min ago.", s / 60),
            s if s < 86_400 => write!(text, " Read {} h ago.", s / 3600),
            s => write!(text, " Read {} d ago.", s / 86_400),
        };
    }
    text
}

/// `agents accounts sessions`: the newest 20 of `clauth sessions`.
///
/// # Errors
/// clauth fails, or its list is not of its shape.
pub fn sessions() -> Result<Vec<legionctl_api::ClauthSession>> {
    #[derive(serde::Deserialize)]
    struct Row {
        id: String,
        workspace: String,
        updated: String,
        last_ran_profile: Option<String>,
    }
    let Some(text) = accounts::clauth(&["sessions", "--json"])? else {
        return Ok(Vec::new());
    };
    let rows: Vec<Row> = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("clauth's session list is not of its shape: {e}"))?;
    rows.into_iter()
        .take(20)
        .map(|r| {
            Ok(legionctl_api::ClauthSession {
                updated_at: accounts::iso_seconds(&r.updated).ok_or_else(|| {
                    anyhow::anyhow!(
                        "clauth's session list is not of its shape: time {}",
                        r.updated
                    )
                })?,
                id: r.id,
                workspace: r.workspace,
                profile: r.last_ran_profile,
            })
        })
        .collect()
}

/// What a wake does with its member's account.
pub enum Decision {
    /// It runs on its account (none: the current login, which clauth's auto-switch moves along its chain).
    Stay,
    /// Its account is at its limit: it continues on `to`.
    Move { to: String, why: String },
}

/// What an operator member of `agent` on `account` does at a wake, along clauth's fallback chain: when its account is
/// a chain member with a window at the chain's threshold or more, it moves to the next member after it (in chain
/// order, around) that is under its threshold and has a use clauth read. A member on the current login, or on an
/// account outside the chain, stays.
///
/// # Errors
/// clauth is not installed or fails, clauth does not show the account, or the account is at its limit and no other
/// chain member is under its threshold (the wake fails with this reason).
pub fn decide(agent: &str, account: Option<&str>) -> Result<Decision> {
    let Some(account) = account else {
        return Ok(Decision::Stay);
    };
    let Some(status) = accounts::status()? else {
        bail!("clauth is not installed, so account {account} cannot run: Set up installs it");
    };
    let chain: Vec<&ProfileStatus> = {
        let mut c: Vec<&ProfileStatus> = status
            .profiles
            .iter()
            .filter(|p| p.harness == agent && p.fallback.is_some())
            .collect();
        c.sort_by_key(|p| p.fallback.as_ref().map(|f| f.position));
        c
    };
    let Some(me) = status
        .profiles
        .iter()
        .find(|p| p.name == account && p.harness == agent)
    else {
        bail!(
            "clauth does not show account {account} of {}",
            accounts::named(agent)
        );
    };
    let Some(full) = me.at_limit() else {
        return Ok(Decision::Stay);
    };
    let threshold = me.fallback.as_ref().map_or(0.0, |f| f.threshold);
    let at = chain.iter().position(|p| p.name == account).unwrap_or(0);
    let mut why = Vec::new();
    for next in chain
        .iter()
        .cycle()
        .skip(at + 1)
        .take(chain.len().saturating_sub(1))
    {
        match (
            next.at_limit(),
            next.windows.is_empty() || next.auth_status == "broken",
        ) {
            (Some(limit), _) => why.push(format!("{}: {limit}", next.name)),
            (None, true) => why.push(format!("{}: clauth has no use of it", next.name)),
            (None, false) => {
                return Ok(Decision::Move {
                    to: next.name.clone(),
                    why: format!(
                        "account {account} has {full} (clauth's fallback chain moves on at {threshold:.0}%)"
                    ),
                });
            }
        }
    }
    bail!(
        "account {account} has {full} (clauth's fallback chain moves on at {threshold:.0}%) and no other account of \
         the chain is under its limit: {}",
        if why.is_empty() {
            "the chain has no other account".to_string()
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
