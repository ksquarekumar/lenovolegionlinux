//! Hook bindings: which actions run when an agent harness fires a hook event.
//!
//! The agent plugin calls one dispatcher (`legion hooks run --event <event>`); what it runs is data in the operator
//! database, so users change it without editing the plugin. Actions are built in (reminders, load notices, setup
//! check) or user-defined shell commands with a timeout. One failing action never stops the others.

use std::process::Command;

use anyhow::{Result, bail, ensure};
use rusqlite::params;
use schemars::JsonSchema;
use serde::Serialize;

use crate::db::Db;
use crate::reminders::{self, Event};
use crate::setup;
use legionctl_machine::{load, sysroot::SysRoot};

/// (id, what it does)
pub const BUILTIN_ACTIONS: &[(&str, &str)] = &[
    (
        "reminders",
        "Inject the enabled reminders for this event (see `legion reminders`).",
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

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Action {
    pub id: String,
    pub builtin: bool,
    /// Description for built-ins; the shell command for user-defined actions.
    pub detail: String,
    pub timeout_s: u32,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Binding {
    pub event: Event,
    pub action: String,
    pub position: i64,
}

fn event_name(event: Event) -> &'static str {
    match event {
        Event::SessionStart => "session-start",
        Event::Prompt => "prompt",
    }
}

fn parse_event(name: &str) -> Option<Event> {
    match name {
        "session-start" => Some(Event::SessionStart),
        "prompt" => Some(Event::Prompt),
        _ => None,
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

pub struct Hooks<'a> {
    pub db: &'a Db,
    pub reminders: reminders::Store,
    /// Who makes changes, for the event log (cli, mcp, ...).
    pub actor: &'a str,
}

impl Hooks<'_> {
    pub fn actions(&self) -> Result<Vec<Action>> {
        let mut all: Vec<Action> = BUILTIN_ACTIONS
            .iter()
            .map(|&(id, what)| Action {
                id: id.into(),
                builtin: true,
                detail: what.into(),
                timeout_s: 5,
            })
            .collect();
        let mut stmt = self
            .db
            .conn()
            .prepare("SELECT id, command, timeout_s FROM hook_actions ORDER BY id")?;
        let custom = stmt.query_map([], |r| {
            Ok(Action {
                id: r.get(0)?,
                builtin: false,
                detail: r.get(1)?,
                timeout_s: r.get(2)?,
            })
        })?;
        for action in custom {
            all.push(action?);
        }
        Ok(all)
    }

    pub fn bindings(&self) -> Result<Vec<Binding>> {
        let mut stmt = self.db.conn().prepare(
            "SELECT event, action, position FROM hook_bindings ORDER BY event, position",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (event, action, position) = row?;
            if let Some(event) = parse_event(&event) {
                out.push(Binding {
                    event,
                    action,
                    position,
                });
            }
        }
        Ok(out)
    }

    fn action_exists(&self, id: &str) -> Result<bool> {
        Ok(self.actions()?.iter().any(|a| a.id == id))
    }

    /// Bind an action to an event; it runs after the ones already bound.
    pub fn bind(&self, event: Event, action: &str) -> Result<()> {
        ensure!(
            self.action_exists(action)?,
            "no action named {action}; see `legion hooks actions`"
        );
        let next: i64 = self.db.conn().query_row(
            "SELECT coalesce(max(position) + 1, 0) FROM hook_bindings WHERE event = ?1",
            [event_name(event)],
            |r| r.get(0),
        )?;
        let changed = self.db.conn().execute(
            "INSERT OR IGNORE INTO hook_bindings (event, action, position) VALUES (?1, ?2, ?3)",
            params![event_name(event), action, next],
        )?;
        ensure!(
            changed == 1,
            "{action} is already bound to {}",
            event_name(event)
        );
        self.db.record(
            self.actor,
            "hooks.bind",
            &serde_json::json!({"event": event_name(event), "action": action}),
        )?;
        Ok(())
    }

    pub fn unbind(&self, event: Event, action: &str) -> Result<()> {
        let changed = self.db.conn().execute(
            "DELETE FROM hook_bindings WHERE event = ?1 AND action = ?2",
            params![event_name(event), action],
        )?;
        ensure!(
            changed == 1,
            "{action} is not bound to {}",
            event_name(event)
        );
        self.db.record(
            self.actor,
            "hooks.unbind",
            &serde_json::json!({"event": event_name(event), "action": action}),
        )?;
        Ok(())
    }

    /// Define a shell-command action. It runs as you, with `sh -c`, under a timeout; its stdout goes to the agent.
    pub fn add_command(&self, id: &str, command: &str, timeout_s: u32) -> Result<()> {
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
        self.db.conn().execute(
            "INSERT INTO hook_actions (id, command, timeout_s) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET command = excluded.command, timeout_s = excluded.timeout_s",
            params![id, command.trim(), timeout_s],
        )?;
        self.db.record(
            self.actor,
            "hooks.action.set",
            &serde_json::json!({"id": id, "command": command, "timeout_s": timeout_s}),
        )?;
        Ok(())
    }

    /// Remove a user-defined action and its bindings.
    pub fn remove_action(&self, id: &str) -> Result<()> {
        if BUILTIN_ACTIONS.iter().any(|b| b.0 == id) {
            bail!("{id} is built in; unbind it instead");
        }
        let changed = self
            .db
            .conn()
            .execute("DELETE FROM hook_actions WHERE id = ?1", [id])?;
        ensure!(changed == 1, "no user-defined action named {id}");
        self.db
            .conn()
            .execute("DELETE FROM hook_bindings WHERE action = ?1", [id])?;
        self.db.record(
            self.actor,
            "hooks.action.remove",
            &serde_json::json!({"id": id}),
        )?;
        Ok(())
    }

    fn run_action(&self, action: &Action, event: Event) -> String {
        let result: Result<String> = match action.id.as_str() {
            "reminders" => self.reminders.emit(event),
            "load" => Ok(load::brief(&load::read_load(&SysRoot::from_env()))),
            "load-if-busy" => {
                let l = load::read_load(&SysRoot::from_env());
                Ok(if l.level >= load::Level::Busy {
                    load::brief(&l)
                } else {
                    String::new()
                })
            }
            "setup-check" => {
                let report = setup::check();
                let todo = report.needs_attention();
                Ok(if todo.is_empty() {
                    String::new()
                } else {
                    let names: Vec<String> = todo
                        .iter()
                        .map(|i| format!("{} for {}", i.what, i.agent))
                        .collect();
                    format!(
                        "legionctl setup: {} need attention ({}). Run /legionctl:setup.",
                        todo.len(),
                        names.join(", ")
                    )
                })
            }
            _ => Command::new("timeout")
                .args([
                    format!("{}s", action.timeout_s),
                    "sh".into(),
                    "-c".into(),
                    action.detail.clone(),
                ])
                .output()
                .map_err(Into::into)
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

    /// Run every action bound to `event`, in order, and return their combined output.
    pub fn run(&self, event: Event) -> Result<String> {
        let actions = self.actions()?;
        let mut out = Vec::new();
        for binding in self.bindings()?.into_iter().filter(|b| b.event == event) {
            if let Some(action) = actions.iter().find(|a| a.id == binding.action) {
                let text = self.run_action(action, event);
                if !text.is_empty() {
                    out.push(text);
                }
            }
        }
        Ok(out.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::temp_db;

    fn hooks<'a>(db: &'a Db, tag: &str) -> Hooks<'a> {
        let dir =
            std::env::temp_dir().join(format!("legionctl-hooks-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Hooks {
            db,
            reminders: reminders::Store {
                path: dir.join("reminders.toml"),
            },
            actor: "test",
        }
    }

    #[test]
    fn custom_command_actions_run_in_order_and_failures_stay_contained() {
        let db = temp_db("hook-run");
        let h = hooks(&db, "run");
        h.unbind(Event::Prompt, "reminders").unwrap();
        h.unbind(Event::Prompt, "load-if-busy").unwrap();
        h.add_command("hello", "echo hello", 2).unwrap();
        h.add_command("broken", "exit 3", 2).unwrap();
        h.add_command("slow", "sleep 5", 1).unwrap();
        for id in ["broken", "slow", "hello"] {
            h.bind(Event::Prompt, id).unwrap();
        }
        let out = h.run(Event::Prompt).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("legionctl hook action broken failed"));
        assert!(
            lines[1].starts_with("legionctl hook action slow failed"),
            "timeout stops slow actions"
        );
        assert_eq!(lines[2], "hello", "later actions still run");
        let logged: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM events WHERE kind LIKE 'hooks.%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(logged, 8, "every change is in the event log");
    }

    #[test]
    fn validation_guards_bindings_and_actions() {
        let db = temp_db("hook-validate");
        let h = hooks(&db, "validate");
        assert!(h.bind(Event::Prompt, "nope").is_err());
        assert!(h.bind(Event::Prompt, "reminders").is_err(), "already bound");
        assert!(
            h.add_command("load", "echo x", 5).is_err(),
            "built-in ids are reserved"
        );
        assert!(h.add_command("x", "echo a\necho b", 5).is_err());
        assert!(h.add_command("x", "echo x", 60).is_err());
        assert!(h.remove_action("reminders").is_err());
        h.add_command("tmp", "true", 1).unwrap();
        h.bind(Event::SessionStart, "tmp").unwrap();
        h.remove_action("tmp").unwrap();
        assert!(
            h.bindings().unwrap().iter().all(|b| b.action != "tmp"),
            "removing an action drops its bindings"
        );
    }
}
