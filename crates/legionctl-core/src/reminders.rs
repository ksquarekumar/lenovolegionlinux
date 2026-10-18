//! Reminders: short instructions that agent hooks inject at session start or on every prompt, so a skill or a
//! working rule stays in force for the whole session. They are config (`reminders.<id>`): the built-in ones come with
//! the defaults; `add`, `on` and `off` write the user layer's managed file.

use anyhow::{Result, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::Resolved;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Event {
    /// Once, when a session starts.
    SessionStart,
    /// With every user prompt. Keep these few and short: they cost tokens on each turn.
    Prompt,
    /// When the agent is about to end its reply; what runs here goes to the agent, which continues before the user
    /// sees the reply.
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Reminder {
    pub id: String,
    pub text: String,
    pub event: Event,
    pub enabled: bool,
    /// Set by the built-in defaults; it can be turned off, not removed.
    pub builtin: bool,
}

fn validate_id(id: &str) -> Result<()> {
    ensure!(
        (1..=32).contains(&id.len())
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
        "reminder id must be 1-32 characters of a-z, 0-9 and '-'"
    );
    Ok(())
}

/// Reminder text is injected into the agent's context: keep it one short line.
fn validate_text(text: &str) -> Result<()> {
    ensure!(!text.trim().is_empty(), "reminder text must not be empty");
    ensure!(
        text.chars().count() <= 300,
        "reminder text must be 300 characters or fewer"
    );
    ensure!(
        !text.contains(['\n', '\r']),
        "reminder text must be one line"
    );
    Ok(())
}

/// A reminder as every layer must give it (checked when the config loads): its id and one short line.
///
/// # Errors
/// The id or the text does not fit.
pub(crate) fn check(id: &str, text: &str) -> Result<()> {
    validate_id(id)?;
    validate_text(text)
}

#[must_use]
pub fn list(config: &Resolved) -> Vec<Reminder> {
    config
        .config
        .reminders
        .iter()
        .map(|(id, r)| Reminder {
            id: id.clone(),
            text: r.text.clone(),
            event: r.event,
            enabled: r.enabled,
            builtin: config.built_in(&format!("reminders.{id}")),
        })
        .collect()
}

/// # Errors
/// No reminder has this id, the key is locked, or the managed file cannot be written.
pub fn set_enabled(config: &Resolved, by: &str, id: &str, enabled: bool) -> Result<()> {
    ensure!(
        config.config.reminders.contains_key(id),
        "no reminder named {id}; see `legionctl reminders list`"
    );
    crate::settings::set_mine(
        config,
        by,
        &format!("reminders.{id}.enabled"),
        Value::Bool(enabled),
    )
}

/// # Errors
/// `id` or `text` is not valid, `id` is a built-in reminder, the key is locked, or the file cannot be written.
pub fn add(config: &Resolved, by: &str, id: &str, text: &str, event: Event) -> Result<()> {
    validate_id(id)?;
    validate_text(text)?;
    ensure!(
        !config.built_in(&format!("reminders.{id}")),
        "{id} is a built-in reminder; turn it on or off instead"
    );
    crate::settings::set_mine(
        config,
        by,
        &format!("reminders.{id}"),
        json!({"text": text.trim(), "event": event, "enabled": true}),
    )
}

/// # Errors
/// `id` is built in or not known, the key is locked, or the managed file cannot be written.
pub fn remove(config: &Resolved, by: &str, id: &str) -> Result<()> {
    ensure!(
        !config.built_in(&format!("reminders.{id}")),
        "{id} is built in and cannot be removed; turn it off instead"
    );
    ensure!(
        config.config.reminders.contains_key(id),
        "no reminder named {id}"
    );
    crate::settings::set_mine(config, by, &format!("reminders.{id}"), Value::Null)
}

/// The built-in reminder to keep the session's dossier; it applies only to a session with a dossier.
const DOSSIER: &str = "dossier-practice";

/// Text for a hook: enabled reminders for this event, one per line, nothing when none apply. `dossier`: the session
/// has a dossier.
#[must_use]
pub fn emit(config: &Resolved, event: Event, dossier: bool) -> String {
    list(config)
        .into_iter()
        .filter(|r| r.enabled && r.event == event && (dossier || r.id != DOSSIER))
        .map(|r| format!("Reminder: {}", r.text))
        .collect::<Vec<_>>()
        .join("\n")
}
