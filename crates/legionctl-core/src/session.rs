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
    /// The session's workspace: the agent's working folder.
    pub workdir: PathBuf,
    /// The session's MCP servers (`mcp.json`): legionctl's own and each one behind the proxy.
    pub mcp: serde_json::Value,
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

/// bwrap arguments that hide the ways out of a sandbox. The runtime folder holds the session bus, systemd's user
/// manager and the `GnuPG`, ssh and keyring agents, so it becomes a private tmpfs (callers bind back what they need);
/// each `hidden` path that exists becomes an empty tmpfs (a folder) or `/dev/null` (a file or socket, like docker's).
pub fn hide(args: &mut Vec<String>, runtime: &Path, hidden: &[String]) {
    args.extend(["--tmpfs".into(), runtime.to_string_lossy().into_owned()]);
    // the real paths, once each: bwrap cannot mount through a symlink like /var/run -> /run
    let mut real: Vec<PathBuf> = hidden
        .iter()
        .filter_map(|h| std::fs::canonicalize(h).ok())
        .collect();
    real.sort();
    real.dedup();
    for path in real {
        let target = path.to_string_lossy().into_owned();
        if path.is_dir() {
            args.extend(["--tmpfs".into(), target]);
        } else {
            args.extend(["--ro-bind".into(), "/dev/null".into(), target]);
        }
    }
}

/// Where the agent works: a repository's worktree, or a plain folder itself (seen through its overlay).
fn agent_dir(ws: &crate::workspace::Workspace) -> PathBuf {
    if ws.is_folder() {
        ws.repo.clone()
    } else {
        ws.path.clone()
    }
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

/// The session's MCP config: legionctl's own server, and every configured server behind `legionctl mcp-proxy` with
/// its allowlist, as resolved now. A package server runs from its hash-checked store folder, without network unless
/// the package allows it.
///
/// # Errors
/// A server names a package that is missing, not fetched or changed since it was fetched.
pub fn mcp_config(config: &Config, store: &crate::packages::Store) -> Result<serde_json::Value> {
    let mut servers = serde_json::Map::new();
    servers.insert(
        "legionctl".into(),
        serde_json::json!({ "command": "legionctl", "args": ["mcp"] }),
    );
    for (name, server) in &config.mcp.servers {
        let mut args = vec!["mcp-proxy".to_string(), "--server".into(), name.clone()];
        for tool in &server.allow {
            args.extend(["--allow".into(), tool.clone()]);
        }
        for (tool, rule) in &server.cache {
            let scope = if rule.workspace { ":workspace" } else { "" };
            args.extend(["--cache".into(), format!("{tool}={}{scope}", rule.ttl_s)]);
        }
        let command = match &server.package {
            Some(package) => {
                let p = config
                    .packages
                    .get(package)
                    .with_context(|| format!("mcp.servers.{name}: no package {package}"))?;
                if !p.network {
                    args.push("--no-network".into());
                }
                store.command(package, p)?
            }
            None => server.command.clone(),
        };
        anyhow::ensure!(
            !command.is_empty(),
            "mcp.servers.{name} has neither a command nor a package"
        );
        args.push("--".into());
        args.extend(command);
        servers.insert(
            name.clone(),
            serde_json::json!({ "command": "legionctl", "args": args }),
        );
    }
    Ok(serde_json::json!({ "mcpServers": servers }))
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
        headless: bool,
    ) -> Result<Launch> {
        // headless: the agent's ACP adapter (session run); else the interactive command (session start)
        let (agents, key) = if headless {
            (&config.session.acp, "session.acp")
        } else {
            (&config.session.agents, "session.agents")
        };
        let command = agents.get(agent).with_context(|| {
            format!(
                "unknown agent {agent} in {key}; configured: {:?}",
                agents.keys()
            )
        })?;
        // a server whose package is missing or changed stops the session before anything is bound
        let mcp_json = mcp_config(
            config,
            &crate::packages::Store::new(crate::packages::Store::default_root()),
        )?;
        let id = new_id()?;
        let place = place_of(dir);
        self.bind(&id, agent, dossier, create, false, Some(&place))?;
        let ws = self.open_workspace(&id, dir, None, &config.workspace.root)?;
        self.db().execute(
            "INSERT INTO sessions (id, agent, dossier, workdir, started) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, agent, dossier, agent_dir(&ws).to_string_lossy(), now()],
        )?;
        self.launch(id, dossier, &ws, config, command, mcp_json)
    }

    /// Prepare an ended session again: the same id, dossier and workspace, and the agent's resume command
    /// (`session.resume`), so the agent continues its conversation.
    ///
    /// # Errors
    /// No such session, its agent cannot resume, or its dossier is held by another live session.
    pub fn resume_session(&self, id: &str, config: &Config) -> Result<Launch> {
        let record = self.session(id)?;
        let command = config.session.resume.get(&record.agent).with_context(|| {
            format!(
                "agent {} cannot resume a session (session.resume)",
                record.agent
            )
        })?;
        let mcp_json = mcp_config(
            config,
            &crate::packages::Store::new(crate::packages::Store::default_root()),
        )?;
        let place = place_of(&record.workdir);
        self.bind(
            id,
            &record.agent,
            &record.dossier,
            false,
            false,
            Some(&place),
        )?;
        let ws = self.open_workspace(id, &record.workdir, None, &config.workspace.root)?;
        self.launch(
            id.to_string(),
            &record.dossier,
            &ws,
            config,
            command,
            mcp_json,
        )
    }

    /// The sandbox and agent command of session `id` working in workspace `ws`.
    fn launch(
        &self,
        id: String,
        dossier: &str,
        ws: &crate::workspace::Workspace,
        config: &Config,
        command: &[String],
        mcp_json: serde_json::Value,
    ) -> Result<Launch> {
        let runtime = PathBuf::from(
            std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?,
        );
        let session_dir = runtime.join("legionctl/sessions").join(&id);
        std::fs::create_dir_all(&session_dir)?;
        let real_git = session_dir.join("git");
        std::fs::write(&real_git, "")?;
        let settings = session_dir.join("claude-settings.json");
        std::fs::write(&settings, claude_settings(config).to_string())?;
        let mcp = session_dir.join("mcp.json");
        std::fs::write(&mcp, mcp_json.to_string())?;

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
        hide(&mut sandbox, &runtime, &config.session.hidden);
        let workdir = agent_dir(ws);
        bind(&mut sandbox, &runtime.join("legionctl"));
        bind(&mut sandbox, &self.root.join(dossier));
        if ws.is_folder() {
            // the folder at its own path, every write into the dossier's layer
            sandbox.extend([
                "--overlay-src".into(),
                workdir.to_string_lossy().into_owned(),
                "--overlay".into(),
            ]);
            for path in [
                crate::folder::upper(&ws.path),
                crate::folder::work(&ws.path),
                workdir.clone(),
            ] {
                sandbox.push(path.to_string_lossy().into_owned());
            }
        } else {
            bind(&mut sandbox, &workdir);
            bind(&mut sandbox, &git_common_dir(&workdir)?);
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
        sandbox.extend(["--chdir".into(), workdir.to_string_lossy().into_owned()]);
        Ok(Launch {
            scope: if config.session.scope {
                vec![
                    "systemd-run".into(),
                    "--user".into(),
                    "--scope".into(),
                    "--quiet".into(),
                    format!("--unit=legionctl-session-{id}"),
                    "--".into(),
                ]
            } else {
                Vec::new()
            },
            sandbox,
            agent: command
                .iter()
                .map(|a| {
                    a.replace("{id}", &id)
                        .replace("{settings}", &settings.to_string_lossy())
                })
                .collect(),
            real_git,
            workdir,
            mcp: mcp_json,
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
