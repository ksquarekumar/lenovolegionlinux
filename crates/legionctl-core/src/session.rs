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
    /// The account profile (`agents accounts add`), if one was chosen.
    pub account: Option<String>,
}

/// What `session start` and `session run` ask for.
#[derive(Debug, Clone)]
pub struct Start<'a> {
    pub agent: &'a str,
    pub dossier: &'a str,
    /// Take the dossier over from another live session.
    pub steal: bool,
    /// The sandbox's token (`LEGIONCTL_TOKEN`), which the operator maps to this session.
    pub token: &'a str,
    /// A folder in the repository (or the plain folder) to work on.
    pub dir: &'a Path,
    /// The agent's ACP adapter (`session run`) instead of its interactive command.
    pub headless: bool,
    pub account: Option<&'a str>,
}

/// Where account profiles live: `~/.local/share/legionctl/accounts/<name>/{claude,codex}`.
#[must_use]
pub fn accounts_root() -> PathBuf {
    crate::packages::Store::default_root().with_file_name("accounts")
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
    /// What the session uses (its dossier, new or not; its workspace, branch and size) and the commands that change
    /// them; the client shows them before the agent starts.
    pub lines: Vec<String>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The state of a session's scope (`systemctl --user is-active`): active, frozen (paused), inactive (also when it
/// ended and is gone), failed, or unknown when systemctl cannot answer.
#[must_use]
pub fn scope_state(id: &str) -> String {
    let unit = format!("legionctl-session-{id}.scope");
    let systemctl = |args: &[&str]| {
        std::process::Command::new("systemctl")
            .arg("--user")
            .args(args)
            .arg(&unit)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .ok()
            .filter(|s| !s.is_empty())
    };
    match systemctl(&["is-active"]).as_deref() {
        Some("active")
            if systemctl(&["show", "--property=FreezerState", "--value"]).as_deref()
                == Some("frozen") =>
        {
            "frozen".into()
        }
        Some(state) => state.into(),
        None => "unknown".into(),
    }
}

/// Whether a session's scope still runs (a paused scope is frozen, and still active).
#[must_use]
pub fn scope_active(id: &str) -> bool {
    matches!(
        scope_state(id).as_str(),
        "active" | "frozen" | "activating" | "reloading"
    )
}

/// The session's dossier lines from the bind (`created ...` for a new one), then its workspace with branch and size,
/// and how to change each.
fn uses(mut bound: Vec<String>, ws: &crate::workspace::Workspace, workdir: &Path) -> Vec<String> {
    bound.push(format!(
        "workspace: {} on {} (base {}), {}",
        workdir.display(),
        ws.branch,
        ws.base,
        legionctl_machine::human_bytes(size(&ws.path))
    ));
    bound.push(
        "change: another dossier with --dossier <name> (a new name makes one); a topic branch with \
         `legionctl workspace branch <topic>`; see all with `legionctl workspace list`"
            .into(),
    );
    bound
}

/// Bytes of the files under `dir`, without its git data; symlinks count as links.
fn size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name() != ".git")
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

/// The files of session `id` in `<runtime>/legionctl/sessions/<id>`: the mount point of the real git, Claude Code's
/// settings and the MCP config.
fn session_files(
    runtime: &Path,
    id: &str,
    config: &Config,
    mcp_json: &serde_json::Value,
) -> Result<[PathBuf; 3]> {
    let dir = runtime.join("legionctl/sessions").join(id);
    std::fs::create_dir_all(&dir)?;
    let files = [
        dir.join("git"),
        dir.join("claude-settings.json"),
        dir.join("mcp.json"),
    ];
    std::fs::write(&files[0], "")?;
    std::fs::write(&files[1], claude_settings(config).to_string())?;
    std::fs::write(&files[2], mcp_json.to_string())?;
    Ok(files)
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

/// The folder of account profile `account`.
///
/// # Errors
/// The name is not a profile name, or no such profile exists.
pub fn account_dir(account: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        crate::dossier::valid_name(account),
        "an account name is a-z, 0-9 and '-', starting with a letter or digit"
    );
    let dir = accounts_root().join(account);
    anyhow::ensure!(
        dir.is_dir(),
        "no account profile {account}; make it with `legionctl agents accounts add {account}`"
    );
    Ok(dir)
}

/// The skills a session gets (`session.skills`): each skill's name and its hash-checked folder in the store; `None`
/// when the user's own skills stay.
///
/// # Errors
/// An entry names no package, leaves its package, has no `SKILL.md` or repeats a name; or its package is not fetched
/// or changed since.
pub fn skills(
    config: &Config,
    store: &crate::packages::Store,
) -> Result<Option<Vec<(String, PathBuf)>>> {
    let Some(entries) = &config.session.skills else {
        return Ok(None);
    };
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries {
        let (package, inner) = entry.split_once('/').unwrap_or((entry, ""));
        let p = config
            .packages
            .get(package)
            .with_context(|| format!("session.skills: no package {package}"))?;
        let inner = Path::new(inner);
        anyhow::ensure!(
            inner
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "session.skills: {entry} leaves its package"
        );
        let dir = store.checked(package, p)?.join(inner);
        anyhow::ensure!(
            dir.join("SKILL.md").is_file(),
            "session.skills: {entry} has no SKILL.md"
        );
        let name = inner
            .file_name()
            .map_or_else(|| package.to_string(), |n| n.to_string_lossy().into_owned());
        anyhow::ensure!(
            out.iter().all(|(n, _)| *n != name),
            "session.skills: two skills are named {name}"
        );
        out.push((name, dir));
    }
    Ok(Some(out))
}

/// Claude Code's and Codex's folders: the account profile's, or `~/.claude` and `~/.codex`.
fn agent_homes(account: Option<&str>) -> Result<[PathBuf; 2]> {
    Ok(if let Some(account) = account {
        let dir = accounts_root().join(account);
        [dir.join("claude"), dir.join("codex")]
    } else {
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
        [home.join(".claude"), home.join(".codex")]
    })
}

/// Copy Claude Code's transcript of session `id` (`projects/<folder>/<id>.jsonl`, and its `<id>/` folder of
/// subagent transcripts) from one config folder to another, so `claude --resume` finds it there. The agent can write
/// these folders, so only regular files and folders are copied: a symlink would bring in a file the sandbox hides.
fn copy_transcript(from: &Path, to: &Path, id: &str) -> Result<()> {
    fn copy_tree(from: &Path, to: &Path) -> Result<()> {
        let kind = std::fs::symlink_metadata(from)?.file_type();
        if kind.is_dir() {
            std::fs::create_dir_all(to)?;
            for entry in std::fs::read_dir(from)? {
                let entry = entry?;
                copy_tree(&entry.path(), &to.join(entry.file_name()))?;
            }
        } else if kind.is_file() {
            std::fs::copy(from, to)?;
        }
        Ok(())
    }
    for project in std::fs::read_dir(from.join("projects"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|p| p.file_type().is_ok_and(|t| t.is_dir()))
    {
        let target = to.join("projects").join(project.file_name());
        for name in [format!("{id}.jsonl"), id.to_string()] {
            let source = project.path().join(&name);
            if std::fs::symlink_metadata(&source).is_ok() {
                std::fs::create_dir_all(&target)?;
                copy_tree(&source, &target.join(&name))?;
            }
        }
    }
    Ok(())
}

/// The skills folders of Claude Code and Codex (the account's, or the home's) as exactly `skills`, read-only.
fn skill_binds(
    sandbox: &mut Vec<String>,
    account: Option<&str>,
    skills: &[(String, PathBuf)],
) -> Result<()> {
    for home in agent_homes(account)? {
        // the mount point must exist: the home is read-only in the sandbox
        let dir = home.join("skills");
        std::fs::create_dir_all(&dir)?;
        sandbox.extend(["--tmpfs".into(), dir.to_string_lossy().into_owned()]);
        for (name, source) in skills {
            sandbox.extend([
                "--ro-bind".into(),
                source.to_string_lossy().into_owned(),
                dir.join(name).to_string_lossy().into_owned(),
            ]);
        }
    }
    Ok(())
}

/// The account's own logins and transcripts instead of `~/.claude` and `~/.codex`: its folders, writable, and the
/// variables that point the agents there.
/// Each agent's login file in an account profile (A1, decided 2026-10-13): at rest in the keyring
/// (`legionctl-login <account>/<agent>`), in the profile only while a session of the account runs.
const LOGINS: [(&str, &str); 2] = [("claude", ".credentials.json"), ("codex", "auth.json")];

fn keyring_get(entry: &str) -> Result<Option<Vec<u8>>> {
    let out = std::process::Command::new("secret-tool")
        .args(["lookup", "legionctl-login", entry])
        .output()
        .context("running secret-tool")?;
    Ok((out.status.success() && !out.stdout.is_empty()).then_some(out.stdout))
}

fn keyring_put(entry: &str, secret: &[u8]) -> Result<()> {
    use std::io::Write as _;
    let mut child = std::process::Command::new("secret-tool")
        .args([
            "store",
            &format!("--label=legionctl login {entry}"),
            "legionctl-login",
            entry,
        ])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .context("running secret-tool")?;
    child
        .stdin
        .take()
        .context("secret-tool stdin")?
        .write_all(secret)?;
    anyhow::ensure!(
        child.wait()?.success(),
        "secret-tool could not store {entry}"
    );
    Ok(())
}

/// Write the account's logins from the keyring into its profile (0600) for a session; a file already there is newer
/// (a session of the account runs) and stays.
fn logins_out(account: &str) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    for (agent, file) in LOGINS {
        let path = accounts_root().join(account).join(agent).join(file);
        if path.exists() {
            continue;
        }
        if let Some(secret) = keyring_get(&format!("{account}/{agent}"))? {
            std::fs::create_dir_all(path.parent().context("no profile folder")?)?;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?
                .write_all(&secret)?;
        }
    }
    Ok(())
}

impl Dossiers {
    /// For each account no live session uses: a login file in its profile (a new login, or tokens an agent refreshed)
    /// goes into the keyring and off the disk. shortcut: a session without a scope (`session.scope` false) counts as
    /// not live, so its login can go while it runs; keep scopes on with accounts.
    ///
    /// # Errors
    /// The keyring or the sessions table fails.
    pub fn settle_logins(&self) -> Result<()> {
        let live: Vec<String> = self
            .sessions()?
            .into_iter()
            .filter(|s| scope_active(&s.id))
            .filter_map(|s| s.account)
            .collect();
        for entry in std::fs::read_dir(accounts_root())
            .into_iter()
            .flatten()
            .flatten()
        {
            let account = entry.file_name().to_string_lossy().into_owned();
            if live.contains(&account) {
                continue;
            }
            for (agent, file) in LOGINS {
                let path = entry.path().join(agent).join(file);
                let Ok(secret) = std::fs::read(&path) else {
                    continue;
                };
                if !secret.is_empty() {
                    keyring_put(&format!("{account}/{agent}"), &secret)?;
                }
                std::fs::remove_file(&path)?;
            }
        }
        Ok(())
    }
}

fn account_binds(sandbox: &mut Vec<String>, account: &str) -> Result<()> {
    for (agent, variable) in [("claude", "CLAUDE_CONFIG_DIR"), ("codex", "CODEX_HOME")] {
        let dir = accounts_root().join(account).join(agent);
        std::fs::create_dir_all(&dir)?;
        bind(sandbox, &dir);
        sandbox.extend([
            "--setenv".into(),
            variable.into(),
            dir.to_string_lossy().into_owned(),
        ]);
    }
    Ok(())
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
        if let Some(url) = &server.url {
            args.extend(["--url".into(), url.clone()]);
            for (header, entry) in &server.keyring_headers {
                let out = std::process::Command::new("secret-tool")
                    .args(["lookup", "legionctl-mcp", entry])
                    .output()
                    .context("secret-tool")?;
                anyhow::ensure!(
                    out.status.success() && !out.stdout.is_empty(),
                    "mcp.servers.{name}: no keyring entry legionctl-mcp {entry} (secret-tool store --label=... legionctl-mcp {entry})"
                );
                args.extend([
                    "--header".into(),
                    format!("{header}={}", String::from_utf8_lossy(&out.stdout).trim()),
                ]);
            }
            servers.insert(
                name.clone(),
                serde_json::json!({ "command": "legionctl", "args": args }),
            );
            continue;
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
            "mcp.servers.{name} has neither a command, a package nor a url"
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
    pub fn start_session(&self, start: &Start, config: &Config) -> Result<Launch> {
        // headless: the agent's ACP adapter (session run); else the interactive command (session start)
        let (agents, key) = if start.headless {
            (&config.session.acp, "session.acp")
        } else {
            (&config.session.agents, "session.agents")
        };
        let agent = start.agent;
        let command = agents.get(agent).with_context(|| {
            format!(
                "unknown agent {agent} in {key}; configured: {:?}",
                agents.keys()
            )
        })?;
        if let Some(account) = start.account {
            account_dir(account)?;
            self.settle_logins()?;
            logins_out(account)?;
        }
        // a server whose package is missing or changed stops the session before anything is bound
        let store = crate::packages::Store::new(crate::packages::Store::default_root());
        let mcp_json = mcp_config(config, &store)?;
        let skills = skills(config, &store)?;
        let id = new_id()?;
        let place = place_of(start.dir);
        let bound = self.bind(&id, agent, start.dossier, start.steal, Some(&place))?;
        let ws = self.open_workspace(&id, start.dir, None, &config.workspace.root)?;
        self.db().execute(
            "INSERT INTO sessions (id, agent, dossier, workdir, started, account, scoped) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, agent, start.dossier, agent_dir(&ws).to_string_lossy(), now(), start.account, config.session.scope],
        )?;
        self.launch(
            id,
            (&ws, bound, start.token),
            config,
            (command, start.account),
            (mcp_json, skills.as_deref()),
        )
    }

    /// Prepare an ended session again: the same id, dossier and workspace, and the agent's resume command
    /// (`session.resume`), so the agent continues its conversation.
    ///
    /// # Errors
    /// No such session, its agent cannot resume, or its dossier is held by another live session.
    pub fn resume_session(
        &self,
        id: &str,
        config: &Config,
        account: Option<&str>,
        token: &str,
    ) -> Result<Launch> {
        let mut record = self.session(id)?;
        let command = config.session.resume.get(&record.agent).with_context(|| {
            format!(
                "agent {} cannot resume a session (session.resume)",
                record.agent
            )
        })?;
        let store = crate::packages::Store::new(crate::packages::Store::default_root());
        let mcp_json = mcp_config(config, &store)?;
        let skills = skills(config, &store)?;
        let place = place_of(&record.workdir);
        let bound = self.bind(id, &record.agent, &record.dossier, false, Some(&place))?;
        // after the bind: a refused resume leaves the session on its account
        if let Some(new) = account.filter(|a| record.account.as_deref() != Some(*a)) {
            account_dir(new)?;
            let [from, _] = agent_homes(record.account.as_deref())?;
            let [to, _] = agent_homes(Some(new))?;
            copy_transcript(&from, &to, id)?;
            self.db().execute(
                "UPDATE sessions SET account = ?1 WHERE id = ?2",
                params![new, id],
            )?;
            record.account = Some(new.to_string());
        }
        if let Some(account) = record.account.as_deref() {
            self.settle_logins()?;
            logins_out(account)?;
        }
        let ws = self.open_workspace(id, &record.workdir, None, &config.workspace.root)?;
        self.launch(
            id.to_string(),
            (&ws, bound, token),
            config,
            (command, record.account.as_deref()),
            (mcp_json, skills.as_deref()),
        )
    }

    /// The sandbox and agent command of session `id` working in workspace `ws`.
    fn launch(
        &self,
        id: String,
        (ws, bound, token): (&crate::workspace::Workspace, Vec<String>, &str),
        config: &Config,
        (command, account): (&[String], Option<&str>),
        (mcp_json, skills): (serde_json::Value, Option<&[(String, PathBuf)]>),
    ) -> Result<Launch> {
        let dossier = ws.dossier.as_str();
        let runtime = PathBuf::from(
            std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?,
        );
        let [real_git, settings, mcp] = session_files(&runtime, &id, config, &mcp_json)?;

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
            let help = std::process::Command::new("bwrap").arg("--help").output()?;
            anyhow::ensure!(
                String::from_utf8_lossy(&help.stdout).contains("--overlay-src"),
                "a plain folder's session needs bubblewrap 0.10 or newer (overlays); this bwrap has none"
            );
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
        sandbox.extend(
            [
                "--setenv",
                "LEGIONCTL_SESSION",
                &id,
                "--setenv",
                "LEGIONCTL_TOKEN",
                token,
            ]
            .map(String::from),
        );
        if let Some(account) = account {
            account_binds(&mut sandbox, account)?;
        }
        if let Some(skills) = skills {
            skill_binds(&mut sandbox, account, skills)?;
        }
        sandbox.extend(["--chdir".into(), workdir.to_string_lossy().into_owned()]);
        Ok(Launch {
            scope: if config.session.scope {
                let unit = format!("--unit=legionctl-session-{id}");
                ["systemd-run", "--user", "--scope", "--quiet", &unit, "--"]
                    .map(String::from)
                    .to_vec()
            } else {
                Vec::new()
            },
            sandbox,
            agent: command
                .iter()
                .map(|a| {
                    a.replace("{id}", &id)
                        .replace("{settings}", &settings.to_string_lossy())
                        .replace("{mcp}", &mcp.to_string_lossy())
                })
                .collect(),
            real_git,
            lines: uses(bound, ws, &workdir),
            workdir,
            mcp: mcp_json,
            id,
        })
    }

    /// # Errors
    /// The database read fails.
    pub fn sessions(&self) -> Result<Vec<SessionRecord>> {
        let mut stmt = self.db().prepare(
            "SELECT id, agent, dossier, workdir, started, account FROM sessions ORDER BY started DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SessionRecord {
                id: r.get(0)?,
                agent: r.get(1)?,
                dossier: r.get(2)?,
                workdir: PathBuf::from(r.get::<_, String>(3)?),
                started: r.get(4)?,
                account: r.get(5)?,
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
                "SELECT id, agent, dossier, workdir, started, account FROM sessions WHERE id = ?1",
                [id],
                |r| {
                    Ok(SessionRecord {
                        id: r.get(0)?,
                        agent: r.get(1)?,
                        dossier: r.get(2)?,
                        workdir: PathBuf::from(r.get::<_, String>(3)?),
                        started: r.get(4)?,
                        account: r.get(5)?,
                    })
                },
            )
            .optional()?;
        found.ok_or_else(|| {
            crate::error::coded(
                crate::error::Code::NotFound,
                format!("no session {id}; see `legionctl session list`"),
            )
        })
    }
}
