//! Sessions (note 14, milestone 1): an agent that legionctl starts itself, so it runs where the dossier's work is and
//! inside a sandbox: `systemd-run --user --scope` (unit `legionctl-session-<id>`), then `bwrap` with the real home
//! read-only and only the workspace, the dossier, the repository's `.git` and the agent's own folders writable,
//! then the agent. `/usr/bin/git` in the sandbox is the git gate (gates.rs).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::{Config, expand_home};
use crate::dossier::{Dossiers, place_of};
use crate::workspace::git_common_dir;

/// One session that legionctl started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionRecord {
    pub id: String,
    pub agent: String,
    pub dossier: String,
    /// The workspace the agent works in.
    pub workdir: PathBuf,
    /// Seconds since the epoch.
    pub started: i64,
}

/// How the client starts a session in its terminal: `scope` + `sandbox` + `agent`, in that order.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Launch {
    pub id: String,
    /// `systemd-run --user --scope ...`: the session's cgroup, for `session stop` and limits.
    pub scope: Vec<String>,
    /// `bwrap ...`: what the agent can write. The client adds the git gate (its own binary over `/usr/bin/git`).
    pub sandbox: Vec<String>,
    pub agent: Vec<String>,
    /// Where the real git is inside the sandbox, for the gate.
    pub real_git: PathBuf,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn new_id() -> Result<String> {
    Ok(std::fs::read_to_string("/proc/sys/kernel/random/uuid")
        .context("reading a random uuid")?
        .trim()
        .to_string())
}

fn bind(args: &mut Vec<String>, path: &Path) {
    let path = path.to_string_lossy().into_owned();
    args.extend(["--bind".into(), path.clone(), path]);
}

/// Claude Code settings for a session (layer 1 of the gates, note 14): the git gate's refusals as `permissions.deny`,
/// so the agent learns them before it runs a command; the git shim in the sandbox stays the authority.
#[must_use]
pub fn claude_settings(config: &Config) -> serde_json::Value {
    let deny: Vec<String> = config
        .gates
        .git
        .deny
        .iter()
        .map(|rule| format!("Bash(git {rule}:*)"))
        .collect();
    serde_json::json!({ "permissions": { "deny": deny } })
}

impl Dossiers {
    /// Prepare a session of `agent` on `dossier` in the repository that contains `dir`: bind the dossier to a new
    /// session id, open its workspace, record the session, and return how to start it.
    ///
    /// # Errors
    /// The agent is not configured, the dossier cannot be bound (held by a live session), or the workspace cannot
    /// be opened.
    pub fn start_session(
        &self,
        agent: &str,
        dossier: &str,
        create: bool,
        dir: &Path,
        config: &Config,
    ) -> Result<Launch> {
        let command = config.session.agents.get(agent).with_context(|| {
            format!(
                "unknown agent {agent}; configured: {:?}",
                config.session.agents.keys()
            )
        })?;
        let id = new_id()?;
        let place = place_of(dir);
        self.bind(&id, agent, dossier, create, false, Some(&place))?;
        let ws = self.open_workspace(&id, dir, None, &config.workspace.root)?;
        self.db().execute(
            "INSERT INTO sessions (id, agent, dossier, workdir, started) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, agent, dossier, ws.path.to_string_lossy(), now()],
        )?;

        let runtime = PathBuf::from(
            std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?,
        );
        let session_dir = runtime.join("legionctl/sessions").join(&id);
        std::fs::create_dir_all(&session_dir)?;
        let real_git = session_dir.join("git");
        std::fs::write(&real_git, "")?;
        let settings = session_dir.join("claude-settings.json");
        std::fs::write(&settings, claude_settings(config).to_string())?;

        // paths under /tmp (tests, other runtimes) are bound after the private /tmp, so they stay visible
        let mut sandbox: Vec<String> = [
            "bwrap",
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            // a private /tmp: the agent cannot change the host's
            "--tmpfs",
            "/tmp",
        ]
        .map(String::from)
        .to_vec();
        for path in [
            &runtime,
            &ws.path,
            &self.root.join(dossier),
            &git_common_dir(&ws.path)?,
        ] {
            bind(&mut sandbox, path);
        }
        for writable in &config.session.writable {
            let path = expand_home(writable)?;
            if path.exists() {
                bind(&mut sandbox, &path);
            }
        }
        sandbox.extend([
            "--ro-bind".into(),
            "/usr/bin/git".into(),
            real_git.to_string_lossy().into_owned(),
        ]);
        sandbox.extend(["--setenv".into(), "LEGIONCTL_SESSION".into(), id.clone()]);
        sandbox.extend(["--chdir".into(), ws.path.to_string_lossy().into_owned()]);
        Ok(Launch {
            scope: vec![
                "systemd-run".into(),
                "--user".into(),
                "--scope".into(),
                "--quiet".into(),
                format!("--unit=legionctl-session-{id}"),
                "--".into(),
            ],
            sandbox,
            agent: command
                .iter()
                .map(|a| {
                    a.replace("{id}", &id)
                        .replace("{settings}", &settings.to_string_lossy())
                })
                .collect(),
            real_git,
            id,
        })
    }

    /// # Errors
    /// The database read fails.
    pub fn sessions(&self) -> Result<Vec<SessionRecord>> {
        let mut stmt = self.db().prepare(
            "SELECT id, agent, dossier, workdir, started FROM sessions ORDER BY started DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SessionRecord {
                id: r.get(0)?,
                agent: r.get(1)?,
                dossier: r.get(2)?,
                workdir: PathBuf::from(r.get::<_, String>(3)?),
                started: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// # Errors
    /// No session has this id, or the database read fails.
    pub fn session(&self, id: &str) -> Result<SessionRecord> {
        let found = self
            .db()
            .query_row(
                "SELECT id, agent, dossier, workdir, started FROM sessions WHERE id = ?1",
                [id],
                |r| {
                    Ok(SessionRecord {
                        id: r.get(0)?,
                        agent: r.get(1)?,
                        dossier: r.get(2)?,
                        workdir: PathBuf::from(r.get::<_, String>(3)?),
                        started: r.get(4)?,
                    })
                },
            )
            .optional()?;
        found.with_context(|| format!("no session {id}; see `legionctl session list`"))
    }
}
