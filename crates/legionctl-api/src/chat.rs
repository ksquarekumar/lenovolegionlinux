//! The chat bar of the thread screen (design 23, C3), the same in the TUI and the app: the `/` commands that
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
/// any other line is an operation of the registry.
#[must_use]
pub fn action(line: &str, commands: &[AgentCommand], screen: &[&str]) -> Action {
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
            crate::cli::operation(&matches, false)?
                .ok_or_else(|| anyhow::anyhow!("not an operation: {command}"))
        });
    match parsed {
        Ok((op, input)) => Action::Operation(op, input),
        Err(e) => Action::Refused(format!("{e:#}")),
    }
}

/// A row of a thread's log for pane 2: time (UTC), member, kind and what it says.
#[must_use]
pub fn log_line(row: &ThreadLogRow) -> String {
    let p = &row.payload;
    let detail = match (p["to"].as_str(), p["text"].as_str()) {
        (Some(to), Some(text)) => format!("→ {to}: {text}"),
        _ => [
            "title",
            "reason",
            "word",
            "archetype",
            "pace",
            "view",
            "account",
        ]
        .iter()
        .find_map(|k| p[k].as_str())
        .map_or_else(|| p.to_string(), String::from),
    };
    format!(
        "{:02}:{:02} {} {} {detail}",
        row.at / 3600 % 24,
        row.at / 60 % 60,
        if row.member.is_empty() {
            "-"
        } else {
            &row.member
        },
        row.kind.trim_start_matches("thread."),
    )
}
