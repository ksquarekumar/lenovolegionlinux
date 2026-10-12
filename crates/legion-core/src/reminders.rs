//! Reminders: short instructions that agent hooks inject at session start or on every prompt, so a skill or a
//! working rule stays in force for the whole session. A built-in catalogue covers legion's skills and habits;
//! users add their own. State lives in ~/.config/legion/reminders.toml.

use std::path::PathBuf;

use anyhow::{Context, Result, bail, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Event {
    /// Once, when a session starts.
    SessionStart,
    /// With every user prompt. Keep these few and short: they cost tokens on each turn.
    Prompt,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Reminder {
    pub id: String,
    pub text: String,
    pub event: Event,
    pub enabled: bool,
    pub builtin: bool,
}

/// (id, text, event, enabled by default)
const BUILTINS: &[(&str, &str, Event, bool)] = &[
    (
        "ste100",
        "Write all user-facing text in ASD-STE100 Simplified Technical English (skill legion:ste100-style).",
        Event::Prompt,
        true,
    ),
    (
        "glanceable",
        "Structure user-facing output for a glance: answer first, then layered detail; exact identifiers; no repetition (skill legion:glanceable-style).",
        Event::Prompt,
        true,
    ),
    (
        "load-aware",
        "Check legion system_load before builds, test suites or parallel jobs, and scale them down when busy.",
        Event::SessionStart,
        false,
    ),
    (
        "worktree-per-task",
        "Do each code-changing task in its own legion worktree (/legion:worktree).",
        Event::SessionStart,
        false,
    ),
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Custom {
    id: String,
    text: String,
    event: Option<Event>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct State {
    /// Built-ins switched away from their default: id -> enabled.
    #[serde(default)]
    builtins: std::collections::BTreeMap<String, bool>,
    #[serde(default)]
    custom: Vec<Custom>,
    /// Custom reminders that are switched off.
    #[serde(default)]
    disabled: Vec<String>,
}

pub struct Store {
    pub path: PathBuf,
}

impl Default for Store {
    fn default() -> Self {
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
        let config =
            std::env::var_os("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from);
        Store {
            path: config.join("legion/reminders.toml"),
        }
    }
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

impl Store {
    fn load(&self) -> Result<State> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("reading {}", self.path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", self.path.display())),
        }
    }

    fn save(&self, state: &State) -> Result<()> {
        std::fs::create_dir_all(self.path.parent().context("reminders path has no parent")?)?;
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(state)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<Reminder>> {
        let state = self.load()?;
        let mut all: Vec<Reminder> = BUILTINS
            .iter()
            .map(|&(id, text, event, default)| Reminder {
                id: id.into(),
                text: text.into(),
                event,
                enabled: *state.builtins.get(id).unwrap_or(&default),
                builtin: true,
            })
            .collect();
        all.extend(state.custom.iter().map(|c| Reminder {
            id: c.id.clone(),
            text: c.text.clone(),
            event: c.event.unwrap_or(Event::SessionStart),
            enabled: !state.disabled.contains(&c.id),
            builtin: false,
        }));
        Ok(all)
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        validate_id(id)?;
        let mut state = self.load()?;
        if BUILTINS.iter().any(|b| b.0 == id) {
            state.builtins.insert(id.into(), enabled);
        } else if state.custom.iter().any(|c| c.id == id) {
            state.disabled.retain(|d| d != id);
            if !enabled {
                state.disabled.push(id.into());
            }
        } else {
            bail!("no reminder named {id}; see `legion reminders`");
        }
        self.save(&state)
    }

    pub fn add(&self, id: &str, text: &str, event: Event) -> Result<()> {
        validate_id(id)?;
        validate_text(text)?;
        let mut state = self.load()?;
        ensure!(
            !BUILTINS.iter().any(|b| b.0 == id),
            "{id} is a built-in reminder; turn it on or off instead"
        );
        state.custom.retain(|c| c.id != id);
        state.disabled.retain(|d| d != id);
        state.custom.push(Custom {
            id: id.into(),
            text: text.trim().into(),
            event: Some(event),
        });
        self.save(&state)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        validate_id(id)?;
        ensure!(
            !BUILTINS.iter().any(|b| b.0 == id),
            "{id} is built in and cannot be removed; turn it off instead"
        );
        let mut state = self.load()?;
        let before = state.custom.len();
        state.custom.retain(|c| c.id != id);
        ensure!(state.custom.len() < before, "no custom reminder named {id}");
        state.disabled.retain(|d| d != id);
        self.save(&state)
    }

    /// Text for a hook: enabled reminders for this event, one per line, nothing when none apply.
    pub fn emit(&self, event: Event) -> Result<String> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|r| r.enabled && r.event == event)
            .map(|r| format!("Reminder: {}", r.text))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> Store {
        let dir =
            std::env::temp_dir().join(format!("legion-reminders-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store {
            path: dir.join("reminders.toml"),
        }
    }

    #[test]
    fn skill_reminders_are_on_by_default_and_emitted_per_event() {
        let s = store("defaults");
        let prompt = s.emit(Event::Prompt).unwrap();
        assert!(
            prompt.contains("legion:ste100-style") && prompt.contains("legion:glanceable-style")
        );
        assert!(
            s.emit(Event::SessionStart).unwrap().is_empty(),
            "habit reminders are opt-in"
        );
    }

    #[test]
    fn toggles_adds_and_removes() {
        let s = store("edit");
        s.set_enabled("ste100", false).unwrap();
        assert!(!s.emit(Event::Prompt).unwrap().contains("ste100-style"));
        s.add(
            "tests-first",
            "Write or update a test before changing behaviour.",
            Event::SessionStart,
        )
        .unwrap();
        assert!(
            s.emit(Event::SessionStart)
                .unwrap()
                .contains("test before changing")
        );
        s.set_enabled("tests-first", false).unwrap();
        assert!(s.emit(Event::SessionStart).unwrap().is_empty());
        s.remove("tests-first").unwrap();
        assert!(s.list().unwrap().iter().all(|r| r.id != "tests-first"));
        assert!(s.remove("ste100").is_err(), "built-ins cannot be removed");
    }

    #[test]
    fn rejects_bad_ids_and_text() {
        let s = store("bad");
        assert!(s.add("Bad Id", "x", Event::Prompt).is_err());
        assert!(s.add("ok", "line one\nline two", Event::Prompt).is_err());
        assert!(s.add("ok", &"x".repeat(301), Event::Prompt).is_err());
        assert!(s.add("ste100", "override", Event::Prompt).is_err());
        assert!(s.set_enabled("nope", true).is_err());
    }
}
