//! Hook bindings: which actions run when an agent harness fires a hook event.
//!
//! The agent plugin calls one dispatcher (`legionctl hook <event>`, which calls `internal.hooks.run`); what it runs is config
//! (`hooks.bindings`, `hooks.actions`), so users change it without editing the plugin. Actions are built in
//! (reminders, load notices, setup check) or user-defined shell commands with a timeout. One failing action never
//! stops the others.

use std::process::Command;

use anyhow::{Result, bail, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::Resolved;
use crate::reminders::{self, Event};
use crate::setup;
use legionctl_machine::{load, sysroot::SysRoot};

/// (id, what it does)
pub const BUILTIN_ACTIONS: &[(&str, &str)] = &[
    (
        "reminders",
        "Inject the enabled reminders for this event (see `legionctl reminders`).",
    ),
    ("load", "One line with the current load level and advice."),
    (
        "load-if-busy",
        "The load line, only when the machine is busy or overloaded.",
    ),
    (
        "setup-check",
        "A line when ctx or legionctl setup for Claude/Codex needs attention.",
    ),
];

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Action {
    pub id: String,
    pub builtin: bool,
    /// Description for built-ins; the shell command for user-defined actions.
    pub detail: String,
    pub timeout_s: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Binding {
    pub event: Event,
    pub action: String,
    pub position: usize,
}

fn event_key(event: Event) -> &'static str {
    match event {
        Event::SessionStart => "session-start",
        Event::Prompt => "prompt",
        Event::Stop => "stop",
    }
}

fn validate_id(id: &str) -> Result<()> {
    ensure!(
        (1..=32).contains(&id.len())
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
        "action id must be 1-32 characters of a-z, 0-9 and '-'"
    );
    Ok(())
}

#[must_use]
pub fn actions(config: &Resolved) -> Vec<Action> {
    let builtin = BUILTIN_ACTIONS.iter().map(|&(id, what)| Action {
        id: id.into(),
        builtin: true,
        detail: what.into(),
        timeout_s: 5,
    });
    let commands = config.config.hooks.actions.iter().map(|(id, a)| Action {
        id: id.clone(),
        builtin: false,
        detail: a.command.clone(),
        timeout_s: a.timeout,
    });
    builtin.chain(commands).collect()
}

/// Bindings in run order per event.
#[must_use]
pub fn bindings(config: &Resolved) -> Vec<Binding> {
    config
        .config
        .hooks
        .bindings
        .iter()
        .flat_map(|(event, ids)| {
            ids.iter().enumerate().map(|(position, action)| Binding {
                event: *event,
                action: action.clone(),
                position,
            })
        })
        .collect()
}

fn bound(config: &Resolved, event: Event) -> Vec<String> {
    config
        .config
        .hooks
        .bindings
        .get(&event)
        .cloned()
        .unwrap_or_default()
}

/// Bind an action to an event; it runs after the ones already bound.
///
/// # Errors
/// `action` does not exist or is already bound, the key is locked, or the managed file cannot be written.
pub fn bind(config: &Resolved, event: Event, action: &str) -> Result<()> {
    ensure!(
        actions(config).iter().any(|a| a.id == action),
        "no action named {action}; see `legionctl hooks list`"
    );
    let mut ids = bound(config, event);
    ensure!(
        !ids.iter().any(|a| a == action),
        "{action} is already bound to {}",
        event_key(event)
    );
    ids.push(action.to_string());
    config.set_user(&format!("hooks.bindings.{}", event_key(event)), json!(ids))
}

/// # Errors
/// `action` is not bound to `event`, the key is locked, or the managed file cannot be written.
pub fn unbind(config: &Resolved, event: Event, action: &str) -> Result<()> {
    let mut ids = bound(config, event);
    let before = ids.len();
    ids.retain(|a| a != action);
    ensure!(
        ids.len() < before,
        "{action} is not bound to {}",
        event_key(event)
    );
    config.set_user(&format!("hooks.bindings.{}", event_key(event)), json!(ids))
}

/// Define a shell-command action. It runs as you, with `sh -c`, under a timeout; its stdout goes to the agent.
///
/// # Errors
/// `id`, `command` or `timeout_s` is not valid, the key is locked, or the managed file cannot be written.
pub fn add_command(config: &Resolved, id: &str, command: &str, timeout_s: u32) -> Result<()> {
    validate_id(id)?;
    ensure!(
        !BUILTIN_ACTIONS.iter().any(|b| b.0 == id),
        "{id} is a built-in action"
    );
    ensure!(
        !command.trim().is_empty() && command.len() <= 500,
        "command must be 1-500 characters"
    );
    ensure!(!command.contains(['\n', '\r']), "command must be one line");
    ensure!(
        (1..=30).contains(&timeout_s),
        "timeout must be 1-30 seconds; hooks block the agent while they run"
    );
    config.set_user(
        &format!("hooks.actions.{id}"),
        json!({"command": command.trim(), "timeout": timeout_s}),
    )
}

/// Remove a user-defined action and its bindings.
///
/// # Errors
/// `id` is built in or not known, a key is locked, or the managed file cannot be written.
pub fn remove_action(config: &Resolved, id: &str) -> Result<()> {
    if BUILTIN_ACTIONS.iter().any(|b| b.0 == id) {
        bail!("{id} is built in; unbind it instead");
    }
    ensure!(
        config.config.hooks.actions.contains_key(id),
        "no user-defined action named {id}"
    );
    for event in config.config.hooks.bindings.keys() {
        if bound(config, *event).iter().any(|a| a == id) {
            unbind(config, *event, id)?;
        }
    }
    config.set_user(&format!("hooks.actions.{id}"), Value::Null)
}

/// Where a command action runs: a repository layer's action in the calling session's sandbox (O-1); the user's own,
/// and a repository's for a caller that is no session (which the user allowed, O-2), on the host.
fn command(config: &Resolved, action: &Action, jail: &mut Jail<'_>) -> Result<Command> {
    let argv = [
        format!("{}s", action.timeout_s),
        "sh".into(),
        "-c".into(),
        action.detail.clone(),
    ];
    let from_repo = config
        .trace
        .get(&format!("hooks.actions.{}.command", action.id))
        .and_then(|settings| settings.last())
        .is_some_and(|s| s.source.layer == "repo");
    if from_repo && let Some(jail) = jail.get()? {
        let argv: Vec<&str> = std::iter::once("timeout")
            .chain(argv.iter().map(String::as_str))
            .collect();
        return Ok(jail.command(&jail.workdir, &argv));
    }
    let mut cmd = Command::new("timeout");
    cmd.args(argv);
    Ok(cmd)
}

/// The calling session's sandbox, made at the first repository action that needs it.
pub struct Jail<'a> {
    make: &'a dyn Fn() -> Result<Option<crate::session::Jail>>,
    asked: bool,
    made: Option<crate::session::Jail>,
}

impl<'a> Jail<'a> {
    /// `make` gives the caller's session sandbox, or `None` for a caller that is no session.
    #[must_use]
    pub fn new(make: &'a dyn Fn() -> Result<Option<crate::session::Jail>>) -> Jail<'a> {
        Jail {
            make,
            asked: false,
            made: None,
        }
    }

    fn get(&mut self) -> Result<Option<&crate::session::Jail>> {
        if !self.asked {
            self.made = (self.make)()?;
            self.asked = true;
        }
        Ok(self.made.as_ref())
    }
}

fn run_action(config: &Resolved, action: &Action, event: Event, jail: &mut Jail<'_>) -> String {
    let result: Result<String> = match action.id.as_str() {
        "reminders" => Ok(reminders::emit(config, event)),
        "load" => {
            load::read_load(&SysRoot::from_env(), &config.config.load).map(|l| load::brief(&l))
        }
        "load-if-busy" => load::read_load(&SysRoot::from_env(), &config.config.load).map(|l| {
            if l.level >= load::Level::Busy {
                load::brief(&l)
            } else {
                String::new()
            }
        }),
        "setup-check" => {
            crate::archetypes::Sources::resolve(&crate::config::Dirs::from_env(), None)
                .and_then(|agents| setup::check(&agents, &config.config.session))
                .map(|report| {
                    // a count, not the list: it goes into each session's context (`legionctl setup check` lists them)
                    match report.needs_attention().len() {
                        0 => String::new(),
                        n => format!(
                            "legionctl setup: {n} need attention (`legionctl setup check` shows each). Run \
                             /legionctl:setup."
                        ),
                    }
                })
        }
        _ => command(config, action, jail)
            .and_then(|mut cmd| Ok(cmd.output()?))
            .map(|out| {
                if out.status.success() {
                    String::from_utf8_lossy(&out.stdout).trim().to_string()
                } else {
                    format!(
                        "legionctl hook action {} failed ({})",
                        action.id, out.status
                    )
                }
            }),
    };
    result.unwrap_or_else(|e| format!("legionctl hook action {} failed: {e}", action.id))
}

/// Run every action bound to `event`, in order, and return their combined output. A failed action is reported in
/// the output; the others still run. `jail` is where a repository layer's command action runs ([`command`]).
#[must_use]
pub fn run(config: &Resolved, event: Event, jail: &mut Jail<'_>) -> String {
    let actions = actions(config);
    bound(config, event)
        .iter()
        .filter_map(|id| actions.iter().find(|a| &a.id == id))
        .map(|action| run_action(config, action, event, jail))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Dirs, resolve};

    #[test]
    fn custom_command_actions_run_in_order_and_failures_stay_contained() {
        let dir = crate::scratch::Scratch::new(
            std::env::temp_dir().join(format!("legionctl-hooks-run-{}", std::process::id())),
        );
        let dirs = Dirs {
            system: dir.join("etc"),
            user: dir.join("user"),
        };
        let config = || resolve(&dirs, None).unwrap();
        unbind(&config(), Event::Prompt, "reminders").unwrap();
        unbind(&config(), Event::Prompt, "load-if-busy").unwrap();
        add_command(&config(), "hello", "echo hello", 2).unwrap();
        add_command(&config(), "broken", "exit 3", 2).unwrap();
        add_command(&config(), "slow", "sleep 5", 1).unwrap();
        for id in ["broken", "slow", "hello"] {
            bind(&config(), Event::Prompt, id).unwrap();
        }
        let out = run(&config(), Event::Prompt, &mut Jail::new(&|| Ok(None)));
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines[0].starts_with("legionctl hook action broken failed"),
            "{out}"
        );
        assert!(
            lines[1].starts_with("legionctl hook action slow failed"),
            "timeout stops slow actions: {out}"
        );
        assert_eq!(lines[2], "hello", "later actions still run");

        remove_action(&config(), "slow").unwrap();
        assert_eq!(
            bound(&config(), Event::Prompt),
            ["broken", "hello"],
            "removing an action drops its bindings"
        );
    }
}
