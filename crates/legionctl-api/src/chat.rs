//! The chat bar of the thread screen (design 23, C3), in the app: the `/` commands that
//! completion offers (the attached agent's ACP commands first, then the screen's own, then every operation of the
//! registry with the CLI's words and its class), what a line does, and the rows of a thread's log for pane 2.

use serde_json::Value;

use crate::{AgentCommand, Class, OPS, OpInfo, ThreadLogRow};

/// One `/` command that completion offers.
pub struct Entry {
    pub text: String,
    pub about: String,
    /// A command of the attached agent.
    pub agent: bool,
}

/// Every command: the agent's (`/agent:<name>`), the screen's own (`screen`: text and description), then the
/// registry's.
#[must_use]
pub fn entries(commands: &[AgentCommand], screen: Vec<(String, String)>) -> Vec<Entry> {
    let agent = commands.iter().map(|c| Entry {
        text: format!("/agent:{}", c.name),
        about: match &c.hint {
            Some(hint) => format!("{} <{hint}> (agent)", c.description),
            None => format!("{} (agent)", c.description),
        },
        agent: true,
    });
    let screen = screen.into_iter().map(|(text, about)| Entry {
        text,
        about,
        agent: false,
    });
    // an agent's call of a workspace operation waits for the user's approval
    let asks = crate::cli::session_from_env().is_some();
    let registry = OPS.iter().filter(|op| !op.internal()).map(move |op| {
        let class = format!("{:?}", op.class).to_lowercase();
        let ask = if asks && op.class == Class::Workspace {
            ", asks the user"
        } else {
            ""
        };
        Entry {
            text: format!("/{}", op.name.replace('.', " ")),
            about: format!("{} [{class}{ask}]", op.summary()),
            agent: false,
        }
    });
    agent.chain(screen).chain(registry).collect()
}

/// The entries that match `text`: a line of one row that starts with `/`.
#[must_use]
pub fn completions(text: &str, entries: Vec<Entry>) -> Vec<Entry> {
    if !text.starts_with('/') || text.contains('\n') {
        return Vec::new();
    }
    entries
        .into_iter()
        .filter(|e| e.text.starts_with(text) || text.starts_with(&format!("{} ", e.text)))
        .collect()
}

/// What a line of the chat bar does.
pub enum Action {
    /// A prompt for the agent: the text it gets.
    Prompt(String),
    /// A command of the screen: its words after the `/`.
    Screen(Vec<String>),
    /// An operation of the registry with its input, parsed by the CLI's own words.
    Operation(&'static OpInfo, Value),
    /// Nothing runs, for this reason.
    Refused(String),
}

/// What `line` does: text without `/` is a prompt; `/agent:<name> ...` is the prompt `/<name> ...` (ACP's
/// convention) when the agent offers `<name>` (`commands`); a first word in `screen` is the screen's own command;
/// any other line is an operation of the registry, filled as the user's (no session; paths against `dir`, the chat
/// bar's folder).
#[must_use]
pub fn action(
    line: &str,
    commands: &[AgentCommand],
    screen: &[&str],
    dir: &std::path::Path,
) -> Action {
    let line = line.trim();
    let Some(command) = line.strip_prefix('/') else {
        return Action::Prompt(line.to_string());
    };
    let words: Vec<String> = command.split_whitespace().map(String::from).collect();
    let Some(first) = words.first() else {
        return Action::Refused("a command after /".into());
    };
    if let Some(name) = first.strip_prefix("agent:") {
        if commands.iter().any(|c| c.name == name) {
            return Action::Prompt(format!("/{}", &command["agent:".len()..]));
        }
        let known: Vec<String> = commands
            .iter()
            .map(|c| format!("/agent:{}", c.name))
            .collect();
        return Action::Refused(if known.is_empty() {
            format!(
                "the agent offers no command {name}: it gives its commands in its first run, so send it a prompt first"
            )
        } else {
            format!(
                "the agent offers no command {name}; it offers {}",
                known.join(", ")
            )
        });
    }
    if screen.contains(&first.as_str()) {
        return Action::Screen(words);
    }
    let parsed = shlex::split(command)
        .ok_or_else(|| anyhow::anyhow!("a quote is not closed"))
        .and_then(|words| {
            let matches = crate::cli::command(Vec::new())
                .try_get_matches_from(std::iter::once("legionctl".to_string()).chain(words))
                .map_err(|e| anyhow::anyhow!("{}", e.render()))?;
            crate::cli::parse(&matches)
                .ok_or_else(|| anyhow::anyhow!("not an operation: {command}"))
        });
    match parsed {
        Ok((op, mut input)) => {
            crate::cli::fill_user_fields(op, &mut input, dir);
            Action::Operation(op, input)
        }
        Err(e) => Action::Refused(format!("{e:#}")),
    }
}

/// What a row of a thread's log says, in words: a message with its addressee, a watch rule as a sentence, else the
/// payload's main field.
#[must_use]
pub fn log_detail(row: &ThreadLogRow) -> String {
    let p = &row.payload;
    if row.kind == "thread.rule" {
        return rule_sentence(p);
    }
    if row.kind == "thread.account" {
        return match (
            p["account"].as_str(),
            p["why"].as_str(),
            p["error"].as_str(),
        ) {
            (Some(account), Some(why), _) => format!("account {account}: {why}"),
            (Some(account), None, _) => format!("account {account}"),
            (None, _, Some(error)) => format!("usage check failed: {error}"),
            _ => p.to_string(),
        };
    }
    if row.kind == "thread.repo" {
        let repo = p["repo"].as_str().unwrap_or_default();
        return match p["branch"].as_str() {
            Some(branch) => format!(
                "works on {repo} at {branch} (from {})",
                p["base"].as_str().unwrap_or_default()
            ),
            None => format!("works on the folder {repo} (an overlay)"),
        };
    }
    if let (Some(commit), Some(message)) = (p["commit"].as_str(), p["message"].as_str()) {
        return format!("{}: {}", &commit[..commit.len().min(8)], short(message));
    }
    match (p["to"].as_str(), p["text"].as_str()) {
        (Some(to), Some(text)) => format!("→ {to}: {text}"),
        _ => [
            "text",
            "error",
            "title",
            "reason",
            "word",
            "archetype",
            "pace",
            "view",
            "account",
            "agent",
        ]
        .iter()
        .find_map(|k| p[k].as_str())
        .map_or_else(|| p.to_string(), String::from),
    }
}

/// The full text of a row whose [`log_detail`] is shortened (a checkpoint's commit message), for a disclosure.
#[must_use]
pub fn log_more(row: &ThreadLogRow) -> Option<String> {
    let message = row.payload["message"].as_str()?;
    (short(message) != message).then(|| message.to_string())
}

/// The first line of `text`, at most 100 characters, cut at a word with "…".
fn short(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    if line.chars().count() <= 100 && !text.contains('\n') {
        return line.to_string();
    }
    let cut: String = line.chars().take(100).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    format!("{}…", cut.trim_end_matches([',', ';', ':']))
}

/// A watch rule of the log (`thread.rule`): "reminder: after 4 Edit/Write/MultiEdit in crates/, expect make lint".
fn rule_sentence(p: &Value) -> String {
    let tools = p["tools"]
        .as_str()
        .unwrap_or_default()
        .split([',', '|'])
        .collect::<Vec<_>>()
        .join("/");
    let path = match p["path"].as_str().unwrap_or_default() {
        "" => String::new(),
        path => format!(" in {path}"),
    };
    let when = if p["reply"].as_bool() == Some(true) {
        format!("before a reply with {tools}{path} ends")
    } else {
        format!("after {} {tools}{path}", p["every"].as_u64().unwrap_or(1))
    };
    let watched = match p["watched"].as_str() {
        Some("main0") | None => String::new(),
        Some(other) => format!(" (watches {other})"),
    };
    format!(
        "{}: {when}, expect {}{watched}",
        p["kind"].as_str().unwrap_or("reminder"),
        p["expect"].as_str().unwrap_or_default()
    )
}
