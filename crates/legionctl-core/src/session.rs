//! Sessions (note 14, milestone 1): an agent that legionctl starts itself, so it runs where the dossier's work is and
//! inside a sandbox: `systemd-run --user --scope` (unit `legionctl-session-<id>`), then `bwrap` in its own PID, IPC and
//! network namespaces with the real home read-only, keys and tokens hidden, and only the workspace, the dossier and
//! what a commit writes in the repository's `.git` writable; the agents' own folders are layers whose writes stay in
//! the session. The only network is the model gateway, through its socket. `/usr/bin/git` in the sandbox is the git
//! gate (gates.rs). A session can also run in a container (W5, note 18): `docker run` of the repository's agent image
//! (`workspace pack`) with the same folders mounted at their own paths and no network but the gateway; the container
//! is then what pause, unpause, stop and the session's state act on.

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
    /// The clauth account it runs on; none: the current login.
    pub account: Option<String>,
    pub inside: Inside,
}

/// Where a session runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Inside {
    /// On this machine, in a bwrap sandbox.
    #[default]
    Host,
    /// In a container of the repository's agent image (`legionctl workspace pack`).
    Container,
    /// In a VM of the repository's disk (`legionctl workspace pack --as vm`), run by `legionctl vm` in the scope.
    Vm,
}

impl Inside {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Inside::Host => "host",
            Inside::Container => "container",
            Inside::Vm => "vm",
        }
    }

    #[must_use]
    pub fn from_db(text: &str) -> Inside {
        match text {
            "container" => Inside::Container,
            "vm" => Inside::Vm,
            _ => Inside::Host,
        }
    }
}

/// How `legionctl vm` runs a session's VM (`<session folder>/vm.json`): the disk, the folders it shares, and what
/// the guest does.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmSpec {
    /// The repository's disk; each session runs on a throwaway layer over it.
    pub disk: PathBuf,
    pub memory_mib: u64,
    pub cpus: u32,
    pub virtiofsd: String,
    /// Host folders shared with virtiofs: tag, folder, read-only.
    pub shares: Vec<(String, PathBuf, bool)>,
    /// The operator socket the guest's connections go to.
    pub socket: PathBuf,
    /// The NVIDIA GPU's IOMMU group goes to the VM for the run (`vfio bind`).
    pub gpu: bool,
    pub guest: GuestSpec,
}

/// What the guest's `legionctl guest` does (`guest.json` in the `legionctl-spec` share): mount, then run the agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GuestSpec {
    /// In order: later mounts go inside earlier ones.
    pub mounts: Vec<GuestMount>,
    pub env: Vec<(String, String)>,
    pub workdir: PathBuf,
    pub uid: u32,
    pub gid: u32,
    /// Where the agent finds the operator: a socket the guest relays to the host.
    pub socket: PathBuf,
    /// Set by `legionctl vm`: the agent's command, and whether it runs in a terminal.
    pub argv: Vec<String>,
    pub interactive: bool,
    /// The gateway's port (`gateway.port`): the guest serves it on 127.0.0.1, relayed to the host's gateway socket.
    pub gateway: Option<u16>,
}

/// The gateway's socket in the runtime folder `legionctl/` (beside the operator's): a container or VM reaches the
/// host's gateway through it, so its own 127.0.0.1:`gateway.port` works as on the host.
pub const GATEWAY_SOCKET: &str = "gateway.sock";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GuestMount {
    /// A virtiofs share at its path.
    Share {
        tag: String,
        path: PathBuf,
        read_only: bool,
    },
    /// A private empty folder owned by the agent's user.
    Empty(PathBuf),
    /// A file written in: the git identity, a writable file's copy.
    File(PathBuf, String),
    /// A file of an earlier mount over another path: read-only (a git link in the workspace), or writable (a login
    /// file, through its hard link in a share).
    Bind {
        from: PathBuf,
        to: PathBuf,
        read_only: bool,
    },
}

/// Where a repository's VM disks live: `~/.local/share/legionctl/vm`.
#[must_use]
pub fn vm_root() -> PathBuf {
    crate::packages::Store::default_root().with_file_name("vm")
}

/// The Windows VM's folder (`~/.local/share/legionctl/windows`): UEFI variables, TPM state, sockets, its state file.
#[must_use]
pub fn windows_root() -> PathBuf {
    vm_root().with_file_name("windows")
}

/// The Windows VM's Looking Glass memory (`/dev/shm`, the user's).
#[must_use]
pub fn looking_glass_file() -> PathBuf {
    PathBuf::from("/dev/shm/legionctl-looking-glass")
}

/// The user service that runs the Windows VM.
pub const WINDOWS_UNIT: &str = legionctl_machine::winvm::UNIT;

/// The VM disk of the repository that holds `dir` (`legionctl workspace pack --as vm` makes it).
#[must_use]
pub fn vm_disk(dir: &Path) -> PathBuf {
    vm_root().join(format!(
        "{}.qcow2",
        crate::images::project_name(&place_of(dir))
    ))
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
    /// The dossier's topic workspace in that repository to work in (a user's thread has its own); none: the
    /// dossier's workspace.
    pub topic: Option<&'a str>,
    /// The agent's ACP adapter (`session run`) instead of its interactive command.
    pub headless: bool,
    pub account: Option<&'a str>,
    pub inside: Inside,
    /// The NVIDIA GPU: its device nodes on the host and in a container, the GPU itself (VFIO) in a VM.
    pub gpu: bool,
    /// A thread member's session (design 23, P6b): `<thread>/<member>`, and the main agent's folder that it sees
    /// as an overlay.
    pub member: Option<(&'a str, &'a Path)>,
    /// The capability sets it requests besides its harness's and its languages' (DESIGN-13).
    pub capabilities: &'a [String],
    /// A member's parent: the main agent's sets; none: the user is the parent.
    pub parent: Option<&'a [String]>,
    /// A bound run's operations ([`bound_mcp`]): the agent's only tools; empty: unbound.
    pub tools: &'a [String],
}

/// How the client starts a session in its terminal: `scope` + `sandbox` + `agent`, in that order.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Launch {
    pub id: String,
    /// `systemd-run --user --scope ...`: the session's cgroup, for `session stop` and limits.
    pub scope: Vec<String>,
    /// `bwrap ...` (the caller adds the git gate: its own legionctl over `/usr/bin/git`), `docker run ...` of the
    /// agent image, whose `/usr/bin/git` is the gate already, or `vm <spec> --`, which the caller runs with its own
    /// legionctl.
    pub sandbox: Vec<String>,
    pub inside: Inside,
    pub agent: Vec<String>,
    /// Where the real git is inside the sandbox, for the gate.
    pub real_git: PathBuf,
    /// The session's workspace: the agent's working folder.
    pub workdir: PathBuf,
    /// The session's MCP servers (`mcp.json`): legionctl's own and each one behind the proxy.
    pub mcp: serde_json::Value,
    /// The operations the agent is bound to ([`Start::tools`]): its only tools; none: unbound.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// What the session uses (its dossier, new or not; its workspace, branch and size) and the commands that change
    /// them; the client shows them before the agent starts.
    pub lines: Vec<String>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The container of session `id`.
fn container_name(id: &str) -> String {
    format!("legionctl-session-{id}")
}

/// The state of session `id` where it runs: active, frozen (paused), inactive (also when it is gone), or the scope's
/// own word.
///
/// # Errors
/// docker or systemctl cannot tell: the state is not known, and a caller must not take the session as gone.
pub fn state(id: &str, inside: Inside) -> Result<String> {
    let why =
        |what: &str, e: &str| anyhow::anyhow!("cannot read the state of session {id}: {what}: {e}");
    // a VM's runner (legionctl vm, QEMU and virtiofsd) is in the session's scope, like a host session
    if inside == Inside::Container {
        let out = std::process::Command::new("docker")
            .args(["inspect", "-f", "{{.State.Status}}", &container_name(id)])
            .output()
            .map_err(|e| why("docker", &e.to_string()))?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() {
            // a container that is gone is inactive; any other failure (the daemon is down) is not known
            let lower = stderr.to_lowercase();
            if lower.contains("no such object") || lower.contains("no such container") {
                return Ok("inactive".into());
            }
            return Err(why("docker inspect", stderr.trim()));
        }
        return Ok(match String::from_utf8_lossy(&out.stdout).trim() {
            "running" => "active",
            "paused" => "frozen",
            _ => "inactive",
        }
        .into());
    }
    let unit = format!("legionctl-session-{id}.scope");
    let systemctl = |args: &[&str]| -> Result<String> {
        let out = std::process::Command::new("systemctl")
            .arg("--user")
            .args(args)
            .arg(&unit)
            .output()
            .map_err(|e| why("systemctl", &e.to_string()))?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // is-active exits non-zero for an inactive unit and still prints its state; no word is a failure
        if text.is_empty() {
            return Err(why(
                &format!("systemctl --user {}", args.join(" ")),
                String::from_utf8_lossy(&out.stderr).trim(),
            ));
        }
        Ok(text)
    };
    let state = systemctl(&["is-active"])?;
    if state == "active" && systemctl(&["show", "--property=FreezerState", "--value"])? == "frozen"
    {
        return Ok("frozen".into());
    }
    Ok(state)
}

/// Whether session `id` still runs (a paused one is frozen, and still runs).
///
/// # Errors
/// Its state cannot be read ([`state`]).
pub fn active(id: &str, inside: Inside) -> Result<bool> {
    Ok(matches!(
        state(id, inside)?.as_str(),
        "active" | "frozen" | "activating" | "reloading"
    ))
}

/// What `control` does to a running session.
#[derive(Debug, Clone, Copy)]
pub enum Control {
    Pause,
    Unpause,
    Stop,
}

/// Pause, unpause or stop session `id` where it runs: its systemd scope (also a VM's), or its container.
///
/// # Errors
/// systemctl or docker refuses (no such scope or container, or it is not in a state for this).
pub fn control(id: &str, inside: Inside, what: Control) -> Result<()> {
    let (program, args) = match inside {
        Inside::Host | Inside::Vm => {
            let verb = match what {
                Control::Pause => "freeze",
                Control::Unpause => "thaw",
                Control::Stop => "stop",
            };
            let unit = format!("legionctl-session-{id}.scope");
            ("systemctl", vec!["--user".to_string(), verb.into(), unit])
        }
        Inside::Container => {
            let verb = match what {
                Control::Pause => "pause",
                Control::Unpause => "unpause",
                Control::Stop => "stop",
            };
            ("docker", vec![verb.to_string(), container_name(id)])
        }
    };
    let out = std::process::Command::new(program)
        .args(&args)
        .output()
        .with_context(|| format!("running {program}"))?;
    anyhow::ensure!(
        out.status.success(),
        "{program} {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
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
        "change: another dossier with --dossier <name> (a new name makes one); a separate topic workspace \
         with `legionctl workspace open --topic <topic>`; see all with `legionctl workspace list`"
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
    (config, manifest): (&Config, &crate::runnables::Manifest),
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
    std::fs::write(&files[1], claude_settings(config, manifest).to_string())?;
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
/// each `hidden` path (`~/` is the home) that exists becomes an empty tmpfs (a folder) or `/dev/null` (a file or
/// socket, like docker's).
///
/// # Errors
/// A `~/` path and no HOME.
pub fn hide(args: &mut Vec<String>, runtime: &Path, hidden: &[String]) -> Result<()> {
    args.extend(["--tmpfs".into(), runtime.to_string_lossy().into_owned()]);
    hide_paths(args, hidden)
}

/// Each `hidden` path that exists as an empty tmpfs (a folder) or `/dev/null` (a file or socket).
fn hide_paths(args: &mut Vec<String>, hidden: &[String]) -> Result<()> {
    // the real paths, once each: bwrap cannot mount through a symlink like /var/run -> /run
    let mut real: Vec<PathBuf> = Vec::new();
    for h in hidden {
        if let Ok(path) = std::fs::canonicalize(expand_home(h)?) {
            real.push(path);
        }
    }
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
    Ok(())
}

/// Each `.env` file (`.env`, `.env.local`, ...) at the top of `dirs`: keys a sandbox must not read.
fn env_files(dirs: &[&Path]) -> Vec<String> {
    dirs.iter()
        .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".env"))
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect()
}

/// Where the agent works: a repository's worktree, or a plain folder itself (seen through its overlay).
fn agent_dir(ws: &crate::workspace::Workspace) -> PathBuf {
    if ws.is_folder() {
        ws.repo.clone()
    } else {
        ws.path.clone()
    }
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

/// The skills folders of Claude Code and Codex as exactly `skills`, read-only.
fn skill_mounts(mounts: &mut Vec<Mount>, skills: &[(String, PathBuf)]) -> Result<()> {
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    for home in [home.join(".claude"), home.join(".codex")] {
        // the mount point must exist: the home is read-only in the sandbox
        let dir = home.join("skills");
        std::fs::create_dir_all(&dir)?;
        mounts.push(Mount::Empty(dir.clone()));
        for (name, source) in skills {
            mounts.push(Mount::ReadOnly(source.clone(), dir.join(name)));
        }
    }
    Ok(())
}

/// The agent of a session, its command and account, and its thread membership (`<thread>/<member>`, the main
/// agent's folder).
struct Who<'a> {
    agent: &'a str,
    command: &'a [String],
    account: Option<&'a str>,
    member: Option<(&'a str, &'a Path)>,
    /// An ACP adapter with no terminal (`session run`).
    headless: bool,
    /// A bound run's operations; empty: unbound.
    tools: &'a [String],
}

/// A thread member sees the main agent's folder with its own empty upper layer, so its checks never change main's
/// files (design 23, B+3, process job). Each other Claude Code session that legionctl starts may lead an agent team
/// (B+ decision 4); a member is never a lead.
fn team_role(
    mounts: &mut Vec<Mount>,
    env: &mut Vec<(String, String)>,
    id: &str,
    (agent, member): (&str, Option<(&str, &Path)>),
) -> Result<()> {
    match member {
        Some((name, main)) => {
            env.push(("LEGIONCTL_MEMBER".into(), name.into()));
            let layer = overlay_layer(id);
            crate::folder::clear(&layer)?;
            for part in ["upper", "work"] {
                std::fs::create_dir_all(layer.join(part))?;
            }
            mounts.push(Mount::Overlay {
                folder: main.to_path_buf(),
                upper: layer.join("upper"),
                work: layer.join("work"),
            });
        }
        None if agent == "claude" => {
            env.push(("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS".into(), "1".into()));
        }
        None => {}
    }
    Ok(())
}

/// The upper layer of a member's overlay of the main agent's folder (`~/.local/state/legionctl/overlay/<session>`):
/// on disk, since a member's build (`target/`) can be larger than the memory of the runtime folder.
fn overlay_layer(id: &str) -> PathBuf {
    crate::db::default_path().with_file_name("overlay").join(id)
}

/// Remove a member's overlay layer after its wake: the next wake sees main's folder as it is then.
///
/// # Errors
/// The layer cannot be removed.
pub fn drop_overlay(id: &str) -> Result<()> {
    let layer = overlay_layer(id);
    crate::folder::clear(&layer)?;
    Ok(std::fs::remove_dir(&layer)?)
}

/// What a session's agent sees besides the host's root (read-only) or its image, in order: later mounts go over
/// earlier ones.
enum Mount {
    /// Writable, at its own path.
    Writable(PathBuf),
    /// A file or folder, writable, at another path.
    Bind(PathBuf, PathBuf),
    /// A file, writable, at a path: a login's real file (a link's target) over the login (`logins_through`), or a
    /// storage file at its own path. A VM shares it through a hard link (virtiofs shares only folders).
    File(PathBuf, PathBuf),
    /// A file or folder, read-only, at another path.
    ReadOnly(PathBuf, PathBuf),
    /// A private empty folder.
    Empty(PathBuf),
    /// A device node at its own path (a grant's `host.devices`): a host or container sandbox only.
    Device(PathBuf),
    /// A plain folder seen at its own path, every write into the dossier's layer (`upper`, `work`): bwrap only.
    Overlay {
        folder: PathBuf,
        upper: PathBuf,
        work: PathBuf,
    },
}

/// `bwrap` arguments of `mounts` and `env`.
fn bwrap_args(args: &mut Vec<String>, mounts: &[Mount], env: &[(String, String)]) {
    let text = |p: &Path| p.to_string_lossy().into_owned();
    for mount in mounts {
        match mount {
            Mount::Writable(p) => args.extend(["--bind".into(), text(p), text(p)]),
            Mount::Bind(from, to) | Mount::File(from, to) => {
                args.extend(["--bind".into(), text(from), text(to)]);
            }
            Mount::ReadOnly(from, to) => args.extend(["--ro-bind".into(), text(from), text(to)]),
            Mount::Empty(p) => args.extend(["--tmpfs".into(), text(p)]),
            Mount::Device(p) => args.extend(["--dev-bind".into(), text(p), text(p)]),
            Mount::Overlay {
                folder,
                upper,
                work,
            } => args.extend([
                "--overlay-src".into(),
                text(folder),
                "--overlay".into(),
                text(upper),
                text(work),
                text(folder),
            ]),
        }
    }
    for (name, value) in env {
        args.extend(["--setenv".into(), name.clone(), value.clone()]);
    }
}

/// `docker run` arguments of `mounts` and `env`.
///
/// # Errors
/// An overlay (a plain folder's workspace), which a container cannot mount.
fn docker_args(args: &mut Vec<String>, mounts: &[Mount], env: &[(String, String)]) -> Result<()> {
    let text = |p: &Path| p.to_string_lossy().into_owned();
    for mount in mounts {
        match mount {
            Mount::Writable(p) => args.extend(["-v".into(), format!("{}:{}", text(p), text(p))]),
            Mount::Bind(from, to) | Mount::File(from, to) => {
                args.extend(["-v".into(), format!("{}:{}", text(from), text(to))]);
            }
            Mount::ReadOnly(from, to) => {
                args.extend(["-v".into(), format!("{}:{}:ro", text(from), text(to))]);
            }
            Mount::Empty(p) => args.extend(["--tmpfs".into(), text(p)]),
            Mount::Device(p) => args.extend(["--device".into(), text(p)]),
            Mount::Overlay { folder, .. } => anyhow::bail!(
                "a plain folder's workspace ({}) cannot run in a container: use a git repository",
                folder.display()
            ),
        }
    }
    for (name, value) in env {
        args.extend(["-e".into(), format!("{name}={value}")]);
    }
    Ok(())
}

/// The NVIDIA GPU's device nodes in a bwrap sandbox (its `--dev` has none): the host's driver does the work.
fn gpu_nodes(args: &mut Vec<String>) -> Result<()> {
    let nodes: Vec<PathBuf> = std::fs::read_dir("/dev")?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("nvidia"))
        })
        .collect();
    anyhow::ensure!(
        !nodes.is_empty(),
        "no NVIDIA device nodes (/dev/nvidia*): is the NVIDIA driver loaded?"
    );
    for node in nodes.iter().chain([&PathBuf::from("/dev/dri")]) {
        let node = node.to_string_lossy().into_owned();
        args.extend(["--dev-bind".into(), node.clone(), node]);
    }
    Ok(())
}

/// `bwrap ...` over the host's root, read-only, in its own PID, IPC and network namespaces (it sees only its own
/// processes and has only a loopback device), with the ways out and each `.env` file of the repository and the
/// workspace hidden, then `mounts` and `env`. A headless sandbox also has a session of its own (no controlling
/// terminal to push input into); an interactive one keeps the terminal, whose input `dev.tty.legacy_tiocsti = 0`
/// guards. With `gpu` it gets the NVIDIA device nodes.
fn bwrap_sandbox(
    ws: &crate::workspace::Workspace,
    (runtime, hidden, headless, gpu): (&Path, &[String], bool, bool),
    mounts: &[Mount],
    env: &[(String, String)],
) -> Result<Vec<String>> {
    // paths under /tmp (tests, other runtimes) are bound after the private /tmp, so they stay visible
    let mut args: Vec<String> = [
        "bwrap",
        "--die-with-parent",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-net",
        "--cap-drop",
        "ALL",
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
    if headless {
        args.push("--new-session".into());
    }
    let mut hidden = hidden.to_vec();
    hidden.extend(env_files(&[&ws.repo, &agent_dir(ws)]));
    // a hidden path inside a folder that a mount replaces (the cache overlay) is hidden again over that mount
    let inside = |h: &String| {
        expand_home(h).is_ok_and(|p| {
            mounts.iter().any(|m| match m {
                Mount::Writable(f) | Mount::Overlay { folder: f, .. } => {
                    p.starts_with(f) && p != *f
                }
                _ => false,
            })
        })
    };
    let (over, below): (Vec<String>, Vec<String>) = hidden.into_iter().partition(inside);
    hide(&mut args, runtime, &below)?;
    if mounts.iter().any(|m| matches!(m, Mount::Overlay { .. })) {
        let help = std::process::Command::new("bwrap").arg("--help").output()?;
        anyhow::ensure!(
            String::from_utf8_lossy(&help.stdout).contains("--overlay-src"),
            "a plain folder's session and a thread member need bubblewrap 0.10 or newer (overlays); this bwrap has none"
        );
    }
    bwrap_args(&mut args, mounts, env);
    hide_paths(&mut args, &over)?;
    args.extend([
        "--chdir".into(),
        agent_dir(ws).to_string_lossy().into_owned(),
    ]);
    if gpu {
        gpu_nodes(&mut args)?;
    }
    Ok(args)
}

/// `docker run ...` of `image` as this user, with a private home, then `mounts` and `env`. The client adds `-t` when
/// it runs the session in a terminal.
fn docker_sandbox(
    (id, image, gateway, gpu): (&str, &str, Option<u16>, bool),
    (runtime, workdir): (&Path, &Path),
    mut mounts: Vec<Mount>,
    env: &[(String, String)],
) -> Result<Vec<String>> {
    let me = std::fs::metadata("/proc/self")?;
    let (uid, gid) = (
        std::os::unix::fs::MetadataExt::uid(&me),
        std::os::unix::fs::MetadataExt::gid(&me),
    );
    let home = std::env::var("HOME").context("HOME is not set")?;
    // no network but the gateway (`legionctl forward` below), no capabilities, no new privileges
    let mut args: Vec<String> = [
        "docker",
        "run",
        "--rm",
        "--init",
        "-i",
        "--network",
        "none",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
    ]
    .map(String::from)
    .to_vec();
    args.extend([
        "--name".into(),
        container_name(id),
        "--user".into(),
        format!("{uid}:{gid}"),
        // the agent's home is private; its folders come back as mounts
        "--tmpfs".into(),
        format!("{home}:uid={uid},gid={gid},mode=0700"),
        "-e".into(),
        format!("HOME={home}"),
        "-e".into(),
        format!("XDG_RUNTIME_DIR={}", runtime.display()),
        "-w".into(),
        workdir.to_string_lossy().into_owned(),
    ]);
    // who commits: the user's git identity, read-only
    let gitconfig = PathBuf::from(&home).join(".gitconfig");
    if gitconfig.is_file() {
        mounts.push(Mount::ReadOnly(gitconfig.clone(), gitconfig));
    }
    docker_args(&mut args, &mounts, env)?;
    if gpu {
        // the host's driver through the NVIDIA Container Toolkit's CDI spec (`nvidia-ctk cdi generate`)
        args.extend(["--device".into(), "nvidia.com/gpu=all".into()]);
    }
    args.push(image.into());
    if let Some(port) = gateway {
        // the image's static client: 127.0.0.1:<port> in the container goes to the host's gateway socket
        args.extend([
            "legionctl".into(),
            "forward".into(),
            port.to_string(),
            runtime
                .join("legionctl")
                .join(GATEWAY_SOCKET)
                .to_string_lossy()
                .into_owned(),
            "--".into(),
        ]);
    }
    Ok(args)
}

/// What a session's sandbox runs from.
enum Runs {
    Host,
    /// The repository's agent image.
    Container(String),
    /// The repository's VM disk.
    Vm(PathBuf),
}

/// Where a session of the repository that holds `dir` runs, or the error that says how to make its image or disk.
fn runs(inside: Inside, dir: &Path) -> Result<Runs> {
    Ok(match inside {
        Inside::Host => Runs::Host,
        Inside::Container => Runs::Container(packed(dir)?),
        Inside::Vm => {
            let disk = vm_disk(dir);
            anyhow::ensure!(
                disk.is_file(),
                "no VM disk {}: make it with `legionctl workspace pack --as vm` in the repository",
                disk.display()
            );
            Runs::Vm(disk)
        }
    })
}

/// `vm <session folder>/vm.json --` (a `legionctl` subcommand): the session's VM, sharing `mounts` with virtiofs. The runtime folder
/// becomes the guest's own (its operator socket is a relay to the host's), with only the session's files shared.
fn vm_sandbox(
    id: &str,
    disk: &Path,
    vm: &crate::config::VmConfig,
    (runtime, gateway, gpu): (&Path, Option<u16>, bool),
    (workdir, mounts, env): (&Path, Vec<Mount>, &[(String, String)]),
) -> Result<Vec<String>> {
    anyhow::ensure!(
        !gpu || vm_gpu_mark(disk).exists(),
        "the VM disk {} has no NVIDIA driver: make it with `legionctl workspace pack --as vm --gpu`",
        disk.display()
    );
    let me = std::fs::metadata("/proc/self")?;
    let home = PathBuf::from(std::env::var("HOME").context("HOME is not set")?);
    let legionctl = runtime.join("legionctl");
    let mut shares: Vec<(String, PathBuf, bool)> = Vec::new();
    let mut share = |host: PathBuf, path: PathBuf, read_only: bool| {
        let tag = format!("m{}", shares.len());
        shares.push((tag.clone(), host, read_only));
        GuestMount::Share {
            tag,
            path,
            read_only,
        }
    };
    let mut guest_mounts = vec![GuestMount::Empty(home.clone())];
    for mount in mounts {
        match mount {
            // the guest's runtime folder is its own: its operator and gateway sockets relay to the host's
            Mount::ReadOnly(from, _) if from == legionctl => {
                guest_mounts.push(GuestMount::Empty(from));
            }
            // virtiofs shares only folders: a writable file (the session's ~/.claude.json) is the VM's own copy
            Mount::Bind(from, to) if from.is_file() => {
                guest_mounts.push(GuestMount::File(to, std::fs::read_to_string(&from)?));
            }
            // virtiofs shares only folders: a writable file goes through a hard link in a folder of its own
            Mount::File(real, at) => {
                let (folder, link) = through(id, guest_mounts.len(), &real)?;
                guest_mounts.push(share(folder.clone(), folder, false));
                guest_mounts.push(GuestMount::Bind {
                    from: link,
                    to: at,
                    read_only: false,
                });
            }
            Mount::Writable(p) => guest_mounts.push(share(p.clone(), p, false)),
            Mount::Bind(from, to) => guest_mounts.push(share(from, to, false)),
            Mount::ReadOnly(from, to) if from.is_dir() => guest_mounts.push(share(from, to, true)),
            // a file at its own path (a git link in the workspace) stays the file, read-only; another is a copy
            Mount::ReadOnly(from, to) => guest_mounts.push(if from == to {
                GuestMount::Bind {
                    from,
                    to,
                    read_only: true,
                }
            } else {
                GuestMount::File(to, std::fs::read_to_string(&from)?)
            }),
            Mount::Empty(p) => guest_mounts.push(GuestMount::Empty(p)),
            // the grant refuses a device in a VM already
            Mount::Device(p) | Mount::Overlay { folder: p, .. } => anyhow::bail!(
                "{} cannot be in a VM: a VM gets no host device node, and a plain folder's workspace needs a git repository",
                p.display()
            ),
        }
    }
    let gitconfig = home.join(".gitconfig");
    if gitconfig.is_file() {
        guest_mounts.push(GuestMount::File(
            gitconfig.clone(),
            std::fs::read_to_string(&gitconfig)?,
        ));
    }
    let mut guest_env = env.to_vec();
    guest_env.extend([
        // the disk's git gate (workspace pack --as vm) keeps the real git here
        (
            "LEGIONCTL_REAL_GIT".into(),
            "/usr/local/libexec/legionctl/git".into(),
        ),
        ("HOME".into(), home.to_string_lossy().into_owned()),
        (
            "XDG_RUNTIME_DIR".into(),
            runtime.to_string_lossy().into_owned(),
        ),
    ]);
    let spec = VmSpec {
        disk: disk.to_path_buf(),
        memory_mib: vm.memory_mib,
        cpus: vm.cpus,
        virtiofsd: vm.virtiofsd.clone(),
        shares,
        socket: legionctl.join("api.sock"),
        gpu,
        guest: GuestSpec {
            mounts: guest_mounts,
            env: guest_env,
            workdir: workdir.to_path_buf(),
            uid: std::os::unix::fs::MetadataExt::uid(&me),
            gid: std::os::unix::fs::MetadataExt::gid(&me),
            socket: legionctl.join("api.sock"),
            gateway,
            ..GuestSpec::default()
        },
    };
    let path = legionctl.join("sessions").join(id).join("vm.json");
    std::fs::write(&path, serde_json::to_string_pretty(&spec)?)?;
    // the caller runs it with its own legionctl, as it adds the git gate on the host
    Ok(vec![
        "vm".into(),
        path.to_string_lossy().into_owned(),
        "--".into(),
    ])
}

/// A folder of its own for the VM's share of login file `p`, and the hard link of `p` in it: the guest binds the link
/// over the file, and a write in place reaches the real file.
fn through(id: &str, n: usize, p: &Path) -> Result<(PathBuf, PathBuf)> {
    let folder = session_home(id).join(format!("through-{n}"));
    let link = folder.join(p.file_name().context("a login file has no name")?);
    crate::folder::clear(&folder)?;
    // clear makes no folder that is not there (a first start)
    std::fs::create_dir_all(&folder)?;
    std::fs::hard_link(p, &link).with_context(|| format!("linking {} for the VM", p.display()))?;
    Ok((folder, link))
}

/// The mark beside a VM disk that `workspace pack --as vm --gpu` made: its guest has the NVIDIA driver.
#[must_use]
pub fn vm_gpu_mark(disk: &Path) -> PathBuf {
    disk.with_extension("gpu")
}

/// The agent image of the repository that holds `dir` (`legionctl workspace pack` makes it).
#[must_use]
pub fn agent_image(dir: &Path) -> String {
    crate::images::image(
        &crate::images::project_name(&place_of(dir)),
        "legionctl-agent",
        "latest",
    )
}

/// Session `id`'s agent image, or the error that says how to make it.
fn packed(dir: &Path) -> Result<String> {
    let image = agent_image(dir);
    let found = std::process::Command::new("docker")
        .args(["image", "inspect", "-f", "{{.Id}}", &image])
        .output()
        .context("running docker")?;
    anyhow::ensure!(
        found.status.success(),
        "no agent image {image}: make it with `legionctl workspace pack` in the repository"
    );
    Ok(image)
}

/// clauth has `account`, with a login of `agent` when it is Claude Code or Codex (an agent of another name gets the
/// logins that the account has).
fn check_account(account: &str, agent: &str) -> Result<()> {
    if LOGINS.iter().any(|(a, _)| *a == agent) {
        crate::accounts::profile_login(account, agent).map(drop)
    } else {
        crate::accounts::exists(account)
    }
}

/// Each agent's login file in its home folder.
pub(crate) const LOGINS: [(&str, &str); 2] =
    [("claude", ".credentials.json"), ("codex", "auth.json")];

/// A bound run's MCP config (DESIGN-11: a bound agent gets only what its binding needs): legionctl's server with
/// the operations `tools` only; no tools: the session's servers `mcp`.
fn bound_mcp(mcp: serde_json::Value, tools: &[String]) -> serde_json::Value {
    if tools.is_empty() {
        return mcp;
    }
    let mut args = vec!["mcp".to_string()];
    for tool in tools {
        args.extend(["--only".into(), tool.clone()]);
    }
    serde_json::json!({ "mcpServers": { "legionctl": { "command": "legionctl", "args": args } } })
}

/// Codex's config in a bound run: no hosted web search, image generation, apps, plugins, hooks, memories or browser
/// and computer use; the MCP servers come from the ACP client only.
const CODEX_BOUND_CONFIG: &str = r#"web_search = "disabled"
[features]
image_generation = false
apps = false
plugins = false
tool_suggest = false
hooks = false
memories = false
browser_use = false
computer_use = false
"#;

/// A bound run's own Codex home (`session-home/<id>/codex-bound`), so Codex reads none of the user's config, plugins,
/// hooks or MCP servers: its config ([`CODEX_BOUND_CONFIG`]), `environments.toml` with no default environment (Codex
/// then gives the model no shell, `apply_patch` or `view_image`; the local one stays for the stdio MCP servers), and
/// the Codex login of clauth account `account` (else the current login) as the real file (DESIGN-12 wall 1). Claude Code ignores it; its binding
/// is the ACP `_meta` of the run. No tools: unbound, nothing.
fn codex_bound_home(
    mounts: &mut Vec<Mount>,
    env: &mut Vec<(String, String)>,
    id: &str,
    (account, tools): (Option<&str>, &[String]),
) -> Result<()> {
    if tools.is_empty() {
        return Ok(());
    }
    let dir = session_home(id).join("codex-bound");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("config.toml"), CODEX_BOUND_CONFIG)?;
    std::fs::write(dir.join("environments.toml"), "default = \"none\"\n")?;
    mounts.push(Mount::Writable(dir.clone()));
    // the login as `logins_through` binds it: the clauth account's own file, else the current login's target
    let live = crate::accounts::live_file("codex")?;
    let real = match account.and_then(|a| crate::accounts::profile_login(a, "codex").ok()) {
        Some(own) => Some(own),
        None if live.is_file() => Some(
            live.canonicalize()
                .with_context(|| format!("reading the link {}", live.display()))?,
        ),
        None => None,
    };
    if let Some(real) = real {
        let point = dir.join("auth.json");
        std::fs::write(&point, b"")?;
        mounts.push(Mount::File(real, point));
    }
    env.retain(|(name, _)| name != "CODEX_HOME");
    env.push(("CODEX_HOME".into(), dir.to_string_lossy().into_owned()));
    Ok(())
}

/// Claude Code settings for a session (layer 1 of the gates, note 14): the git gate's refusals and the grant's
/// forbidden commands as `permissions.deny`, so the agent learns them before it runs a command (the git shim in the
/// sandbox stays the authority for git), with the harness set's settings over them (its own sandbox off).
fn claude_settings(config: &Config, manifest: &crate::runnables::Manifest) -> serde_json::Value {
    let deny: Vec<String> = config
        .gates
        .git
        .deny
        .iter()
        .map(|rule| format!("git {rule}"))
        .chain(manifest.host.forbidden_commands.iter().cloned())
        .map(|command| format!("Bash({command}:*)"))
        .collect();
    let mut settings = serde_json::json!({ "permissions": { "deny": deny } });
    if let Some(harness) = &manifest.harness {
        merge_json(&mut settings, &harness.settings);
    }
    settings
}

/// `over` into `into`: objects merge key by key, any other value replaces.
fn merge_json(into: &mut serde_json::Value, over: &serde_json::Value) {
    match (into, over) {
        (serde_json::Value::Object(into), serde_json::Value::Object(over)) => {
            for (key, value) in over {
                merge_json(
                    into.entry(key.clone()).or_insert(serde_json::Value::Null),
                    value,
                );
            }
        }
        (_, serde_json::Value::Null) => {}
        (into, over) => *into = over.clone(),
    }
}

/// The environment variable of `legionctl mcp-proxy` with the remote server's keyring headers, a JSON object of name
/// and value.
pub const HEADERS_ENV: &str = "LEGIONCTL_MCP_HEADERS";

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
            // secret values go in the proxy's environment, never in its command line (the exec sensor records argv)
            let mut headers = serde_json::Map::new();
            for (header, entry) in &server.keyring_headers {
                let out = std::process::Command::new("secret-tool")
                    .args(["lookup", "legionctl-mcp", entry])
                    .output()
                    .context("secret-tool")?;
                anyhow::ensure!(
                    out.status.success() && !out.stdout.is_empty(),
                    "mcp.servers.{name}: no keyring entry legionctl-mcp {entry} (secret-tool store --label=... legionctl-mcp {entry})"
                );
                headers.insert(
                    header.clone(),
                    String::from_utf8_lossy(&out.stdout).trim().into(),
                );
            }
            let mut entry = serde_json::json!({ "command": "legionctl", "args": args });
            if !headers.is_empty() {
                entry["env"] = serde_json::json!({ HEADERS_ENV: serde_json::Value::Object(headers).to_string() });
            }
            servers.insert(name.clone(), entry);
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

/// `mcp` with the session's operator variables in the environment of each legionctl server (its own and the proxied
/// ones): Codex starts MCP servers with a cleared environment, so they would reach no operator, or not as the session.
fn session_env(
    mut mcp: serde_json::Value,
    id: &str,
    token: &str,
    runtime: &Path,
) -> serde_json::Value {
    for server in mcp["mcpServers"]
        .as_object_mut()
        .into_iter()
        .flat_map(|s| s.values_mut())
        .filter(|s| s["command"] == "legionctl")
    {
        if !server["env"].is_object() {
            server["env"] = serde_json::json!({});
        }
        server["env"]["LEGIONCTL_SESSION"] = id.into();
        server["env"]["LEGIONCTL_TOKEN"] = token.into();
        server["env"]["XDG_RUNTIME_DIR"] = runtime.to_string_lossy().into_owned().into();
    }
    mcp
}

/// The agents' variables for the model gateway: model calls to its APIs, every other HTTPS call through its tunnel to
/// the session's granted connections (DESIGN-12; Codex on a `ChatGPT` login reaches chatgpt.com only this way). And the
/// line that says what network the session has.
fn network(env: &mut Vec<(String, String)>, gateway: Option<u16>) -> String {
    let Some(port) = gateway else {
        return "network: none (gateway.port is 0: no model gateway)".into();
    };
    env.extend([
        (
            "ANTHROPIC_BASE_URL".into(),
            format!("http://127.0.0.1:{port}"),
        ),
        (
            "OPENAI_BASE_URL".into(),
            format!("http://127.0.0.1:{port}/v1"),
        ),
        ("HTTPS_PROXY".into(), format!("http://127.0.0.1:{port}")),
        ("NO_PROXY".into(), "127.0.0.1,localhost".into()),
    ]);
    format!(
        "network: the model gateway and the granted connections only (127.0.0.1:{port} in the sandbox)"
    )
}

/// For a headless run on the host: make its ACP command ready ([`crate::setup::ready`]: a missing adapter is
/// installed), and give it the PATH it starts with. The operator's PATH is the user manager's from its start: the
/// folders of `setup apply` may be new since.
fn headless_path(env: &mut Vec<(String, String)>, config: &Config, who: &Who) -> Result<()> {
    if who.headless {
        crate::setup::ready(
            &config.session,
            who.command,
            &crate::setup::session_path()?,
            &mut |_| {},
        )?;
        let path = crate::setup::session_path()?;
        env.push((
            "PATH".into(),
            std::env::join_paths(path)?.to_string_lossy().into_owned(),
        ));
    }
    Ok(())
}

/// The agent command behind `legionctl forward`: the sandbox's own 127.0.0.1:`port` goes to the gateway's `socket`,
/// its only way out.
fn forward(agent: &mut Vec<String>, gateway: Option<u16>, socket: &Path) {
    if let Some(port) = gateway {
        agent.splice(
            0..0,
            [
                "legionctl".into(),
                "forward".into(),
                port.to_string(),
                socket.to_string_lossy().into_owned(),
                "--".into(),
            ],
        );
    }
}

/// The mounts of a grant's storage and devices after the session's own (later mounts go over earlier ones), and a
/// line for each one that this sandbox leaves out. A folder that is not there yet is made (cargo makes
/// `~/.cargo/registry` at its first download; the home is read-only in the sandbox).
fn grant_mounts(
    mounts: &mut Vec<Mount>,
    manifest: &crate::runnables::Manifest,
    layer: &Path,
    host: bool,
) -> Result<Vec<String>> {
    use crate::runnables::Mode;
    let mut lines = Vec::new();
    for storage in &manifest.storage {
        let path = expand_home(&storage.path)?;
        match storage.mode {
            Mode::Read if !path.exists() => {
                lines.push(format!("storage {}: not there, not mounted", storage.path));
            }
            Mode::Read => mounts.push(Mount::ReadOnly(path.clone(), path)),
            Mode::Overlay if !host => lines.push(format!(
                "storage {}: an overlay on the host only; this sandbox keeps its own",
                storage.path
            )),
            Mode::Overlay => {
                std::fs::create_dir_all(&path)?;
                layered(mounts, &path, (layer, None), true)?;
            }
            Mode::Write | Mode::WriteThrough if path.is_file() => {
                mounts.push(Mount::File(path.clone(), path));
            }
            Mode::Write | Mode::WriteThrough => {
                if !path.exists() {
                    std::fs::create_dir_all(&path)?;
                }
                mounts.push(Mount::Writable(path));
            }
        }
    }
    mounts.extend(
        manifest
            .host
            .devices
            .iter()
            .map(PathBuf::from)
            .map(Mount::Device),
    );
    Ok(lines)
}

/// The granted harness's variables (into `env`: what turns its telemetry and updates off, and its own) and the
/// arguments after its command.
fn harness_parts(
    manifest: &crate::runnables::Manifest,
    env: &mut Vec<(String, String)>,
) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(harness) = &manifest.harness {
        for t in &harness.telemetry {
            if let Some((name, value)) = t.env.as_deref().and_then(|e| e.split_once('=')) {
                env.push((name.into(), value.into()));
            }
            args.extend(t.args.iter().cloned());
        }
        env.extend(harness.env.iter().map(|(k, v)| (k.clone(), v.clone())));
        args.extend(harness.args.iter().cloned());
    }
    args
}

/// The line that says what the session got: its sets and its connections, which the session record keeps for the
/// gateway's tunnel.
fn granted_line(grant: &crate::runnables::Grant) -> String {
    let sets: Vec<String> = grant
        .sets
        .iter()
        .map(|s| format!("{}@{}", s.name, s.version))
        .collect();
    let c = &grant.manifest.connections;
    format!(
        "capabilities: {}; connections: {} out, web read {} (`legionctl runnables show <session>`)",
        if sets.is_empty() {
            "none".into()
        } else {
            sets.join(", ")
        },
        c.out.len(),
        if c.web_read { "on" } else { "off" }
    )
}

/// A session's own state (`~/.local/state/legionctl/session-home/<id>`): the layers over the home folders it writes
/// (`home/`), kept for a resume, and the throwaway layers of the commands run for it (`run-*`).
fn session_home(id: &str) -> PathBuf {
    crate::db::default_path()
        .with_file_name("session-home")
        .join(id)
}

/// The agents' own state and, on the host, the caches (`session.writable`, `session.caches`), never the real files:
/// on the host each folder is an overlay over the real one with its writes in `layer`, in a container or VM the
/// session's copy in `layer` (made at its first start); each file is the session's copy in `layer`. A path that does
/// not exist is left out.
fn home_mounts(
    mounts: &mut Vec<Mount>,
    config: &Config,
    (layer, account): (&Path, Option<&str>),
    host: bool,
) -> Result<()> {
    let caches: &[String] = if host { &config.session.caches } else { &[] };
    for entry in config.session.writable.iter().chain(caches) {
        let path = expand_home(entry)?;
        if path.exists() {
            layered(mounts, &path, (layer, account), host)?;
        }
    }
    Ok(())
}

/// `path` with every write in the session's `layer`: on the host a folder is an overlay over the real one; in a
/// container or VM (and a file) the session's copy in `layer`, made at its first start.
fn layered(
    mounts: &mut Vec<Mount>,
    path: &Path,
    (layer, account): (&Path, Option<&str>),
    host: bool,
) -> Result<()> {
    let own = layer.join(path.strip_prefix("/").unwrap_or(path));
    if host && path.is_dir() {
        for part in ["upper", "work"] {
            std::fs::create_dir_all(own.join(part))?;
        }
        mounts.push(Mount::Overlay {
            folder: path.to_path_buf(),
            upper: own.join("upper"),
            work: own.join("work"),
        });
        logins_through(mounts, path, &own.join("upper"), account)?;
        return Ok(());
    }
    if !own.exists() {
        std::fs::create_dir_all(own.parent().context("a layer path has no folder")?)?;
        let out = std::process::Command::new("cp")
            .args(["-a", "--reflink=auto", "--"])
            .args([path, &own])
            .output()
            .context("running cp")?;
        anyhow::ensure!(
            out.status.success(),
            "copying {} for the session: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    mounts.push(Mount::Bind(own.clone(), path.to_path_buf()));
    logins_through(mounts, path, &own, account)
}

/// The agents' login files in a home folder of theirs (`.credentials.json` of Claude Code, `auth.json` of Codex) are
/// the real files over the session's layer (DESIGN-12 wall 1): a refresh rotates the refresh token, so a session that
/// wrote it into its layer would leave the spent token in the real file and log the user's own agent out. The real
/// file of a session on clauth account `account` is that profile's own login file (as `clauth start` links it), else
/// the current login; a link (clauth links `~/.claude/.credentials.json` to its active profile's file) is its target,
/// so a clauth switch later changes the link, not the session. A login path that is no plain file (a link, or no
/// login) becomes an empty plain file in `layer` (the overlay's upper folder, or the session's copy), the mount point:
/// no sandbox mounts on a link.
fn logins_through(
    mounts: &mut Vec<Mount>,
    folder: &Path,
    layer: &Path,
    account: Option<&str>,
) -> Result<()> {
    for (agent, file) in LOGINS {
        let login = folder.join(file);
        let own = match account {
            // the session's own agent has its login there (checked at its start); the other agent keeps the
            // current one when the account has none of it
            Some(account) if login == crate::accounts::live_file(agent)? => {
                crate::accounts::profile_login(account, agent).ok()
            }
            _ => None,
        };
        let real = match own {
            Some(own) => own,
            // a link to no file is no login
            None if login.is_file() => login
                .canonicalize()
                .with_context(|| format!("reading the link {}", login.display()))?,
            None => continue,
        };
        if !login
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_file())
        {
            let point = layer.join(file);
            if std::fs::symlink_metadata(&point).is_ok() {
                std::fs::remove_file(&point)?;
            }
            std::fs::write(&point, b"")?;
        }
        mounts.push(Mount::File(real, login));
    }
    Ok(())
}

/// What a commit in worktree `workdir` on `branch` writes in the repository's git folder: the objects, the refs and
/// reflogs folder of its own branch, the stash and the worktree's own folder. The config, hooks and info, the other
/// branches (also those in the folder of its own branch), tags, remotes, replacements and notes, and the files that
/// point git to its folders (the worktree's `.git`, `commondir`, `gitdir`) stay read-only, so that nothing the agent
/// writes runs when the host's git reads it (S-4) and a session cannot move another session's branch (C7).
fn git_mounts(mounts: &mut Vec<Mount>, workdir: &Path, branch: &str) -> Result<()> {
    let common = git_common_dir(workdir)?;
    let own = PathBuf::from(crate::git::text(
        workdir,
        &["rev-parse", "--absolute-git-dir"],
    )?);
    anyhow::ensure!(
        own != common,
        "a session works in a worktree, not in the main checkout {}",
        workdir.display()
    );
    let folder = Path::new(branch)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .with_context(|| {
            format!("the session's branch {branch} has no folder (legionctl/<dossier>)")
        })?;
    let refs = common.join("refs");
    // these namespaces exist, so they are read-only below (a new one in the writable refs folder would be the agent's)
    for space in ["heads", "tags", "remotes", "replace", "notes"] {
        std::fs::create_dir_all(refs.join(space))?;
    }
    // the folder itself, read-only: a container or VM sees nothing of the host but its mounts
    mounts.push(Mount::ReadOnly(common.clone(), common.clone()));
    mounts.push(Mount::Writable(common.join("objects")));
    mounts.push(Mount::Writable(refs.clone()));
    for sub in std::fs::read_dir(&refs)? {
        let sub = sub?.path();
        if sub.is_dir() {
            mounts.push(Mount::ReadOnly(sub.clone(), sub));
        }
    }
    let branches = refs.join("heads").join(folder);
    std::fs::create_dir_all(&branches)?;
    mounts.push(Mount::Writable(branches.clone()));
    // git writes the branch through <branch>.lock in its folder; each other entry there stays read-only
    let mine = Path::new(branch)
        .file_name()
        .context("the session's branch has no name")?;
    loose_siblings(&common, folder, mine)?;
    for entry in std::fs::read_dir(&branches)? {
        let path = entry?.path();
        let name = path.file_name().unwrap_or_default();
        if name != mine && *name != *format!("{}.lock", mine.to_string_lossy()) {
            mounts.push(Mount::ReadOnly(path.clone(), path));
        }
    }
    // the reflogs (a commit and the stash write them; nothing runs from them), as a folder: a VM shares no file
    let logs = common.join("logs");
    std::fs::create_dir_all(&logs)?;
    mounts.push(Mount::Writable(logs));
    mounts.push(Mount::Writable(own.clone()));
    for file in ["commondir", "gitdir", "config.worktree"] {
        let path = own.join(file);
        if path.exists() {
            mounts.push(Mount::ReadOnly(path.clone(), path));
        }
    }
    let link = workdir.join(".git");
    mounts.push(Mount::ReadOnly(link.clone(), link));
    Ok(())
}

/// Write a loose ref for each packed branch in `refs/heads/<folder>` other than `own` that has none (the same value, by
/// git's lock file), so that it can be mounted read-only: else a loose file the agent writes would take its place.
fn loose_siblings(common: &Path, folder: &Path, own: &std::ffi::OsStr) -> Result<()> {
    let prefix = format!("refs/heads/{}/", folder.display());
    let packed = match std::fs::read_to_string(common.join("packed-refs")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        read => read.context("reading packed-refs")?,
    };
    for (oid, name) in packed
        .lines()
        .filter(|l| !l.starts_with(['#', '^']))
        .filter_map(|l| l.split_once(' '))
    {
        let Some(rest) = name.strip_prefix(&prefix) else {
            continue;
        };
        let loose = common.join(name);
        if rest.as_bytes() == own.as_encoded_bytes() || loose.symlink_metadata().is_ok() {
            continue;
        }
        std::fs::create_dir_all(loose.parent().context("a ref has no folder")?)?;
        let lock = loose.with_file_name(format!(
            "{}.lock",
            loose.file_name().unwrap_or_default().to_string_lossy()
        ));
        // git's own protocol: a lock that is there is a git that writes this ref now
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{oid}\n").as_bytes()))
            .with_context(|| format!("writing {}", lock.display()))?;
        std::fs::rename(&lock, &loose).with_context(|| format!("writing {}", loose.display()))?;
    }
    Ok(())
}

/// A sandbox of a session for the commands that legionctl runs for it (O-1): the repository's checks and tests, the
/// hooks of its repository layer and the checkpoint's commit run here, never on the host. It is the session's host
/// sandbox without the runtime folder (no operator, no gateway, no network) and with a throwaway layer over the home
/// folders, which goes when the jail goes.
pub struct Jail {
    argv: Vec<String>,
    /// The session's workspace.
    pub workdir: PathBuf,
    layer: PathBuf,
}

impl Jail {
    /// `argv` in `dir`, inside the sandbox.
    #[must_use]
    pub fn command(&self, dir: &Path, argv: &[&str]) -> std::process::Command {
        let mut cmd = std::process::Command::new(&self.argv[0]);
        cmd.args(&self.argv[1..])
            .arg("--chdir")
            .arg(dir)
            .arg("--")
            .args(argv);
        cmd
    }
}

impl Drop for Jail {
    fn drop(&mut self) {
        // shortcut: a layer that cannot be removed stays in session-home/<id>/run-*; doctor does not list them yet
        if crate::folder::clear(&self.layer).is_ok() {
            let _ = std::fs::remove_dir(&self.layer);
        }
    }
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
            check_account(account, agent)?;
        }
        // a server whose package is missing or changed stops the session before anything is bound
        let store = crate::packages::Store::new(crate::packages::Store::default_root());
        let mcp_json = mcp_config(config, &store)?;
        let skills = skills(config, &store)?;
        let runs = runs(start.inside, start.dir)?;
        let id = new_id()?;
        // a refused set stops the session before anything is bound (DESIGN-13: at start, not at the first call)
        let grant = crate::runnables::grant(
            config,
            &format!("session:{id}"),
            &crate::runnables::Request {
                agent,
                dir: start.dir,
                inside: start.inside,
                names: start.capabilities,
                parent: start.parent,
            },
        )?;
        let place = place_of(start.dir);
        let bound = self.bind(&id, agent, start.dossier, start.steal, Some(&place))?;
        let ws = self.open_workspace(
            start.dossier.to_owned(),
            start.dir,
            None,
            start.topic,
            &config.workspace.root,
        )?;
        self.db().execute(
            "INSERT INTO sessions (id, agent, dossier, workdir, started, account, scoped, inside) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            // a container tells whether its session is live, as a scope does
            params![id, agent, start.dossier, agent_dir(&ws).to_string_lossy(), now(), start.account, config.session.scope || start.inside == Inside::Container, start.inside.as_str()],
        )?;
        // a start that fails here (no GPU, no GPU driver in the disk) holds nothing
        self.keep_grant(&id, &grant)
            .and_then(|()| {
                self.launch(
                    id.clone(),
                    (&ws, bound, start.token),
                    config,
                    &Who {
                        agent,
                        command,
                        account: start.account,
                        member: start.member,
                        headless: start.headless,
                        tools: start.tools,
                    },
                    (mcp_json, skills.as_deref()),
                    (&runs, start.gpu, &grant),
                )
            })
            .inspect_err(|_| {
                let _ = self.release(&id);
            })
    }

    /// Prepare an ended session again: the same id, dossier and workspace, and the agent's resume command
    /// (`session.resume`), so the agent continues its conversation; `tools` binds the run again ([`Start::tools`]).
    ///
    /// # Errors
    /// No such session, its agent cannot resume, or its dossier is held by another live session.
    pub fn resume_session(
        &self,
        id: &str,
        config: &Config,
        account: Option<&str>,
        token: &str,
        (headless, tools): (bool, &[String]),
    ) -> Result<Launch> {
        let mut record = self.session(id)?;
        // a headless session goes on through its agent's ACP adapter, an interactive one through its resume command
        let (commands, key) = if headless {
            (&config.session.acp, "session.acp")
        } else {
            (&config.session.resume, "session.resume")
        };
        let command = commands
            .get(&record.agent)
            .with_context(|| format!("agent {} cannot resume a session ({key})", record.agent))?;
        let store = crate::packages::Store::new(crate::packages::Store::default_root());
        let mcp_json = mcp_config(config, &store)?;
        let skills = skills(config, &store)?;
        let runs = runs(record.inside, &record.workdir)?;
        // the sets it asked for at its start, granted again under today's layers; a session from before grants asks
        // for its harness's and languages' sets
        let before: Option<crate::runnables::Grant> = self
            .fact_value(&crate::runnables::entity(id), "manifest", &[])?
            .map(serde_json::from_value)
            .transpose()?;
        let grant = crate::runnables::grant(
            config,
            &format!("session:{id}"),
            &crate::runnables::Request {
                agent: &record.agent,
                dir: &record.workdir,
                inside: record.inside,
                names: before.as_ref().map_or(&[], |b| &b.requested),
                parent: before.as_ref().and_then(|b| b.parent.as_deref()),
            },
        )?;
        let place = place_of(&record.workdir);
        let bound = self.bind(id, &record.agent, &record.dossier, false, Some(&place))?;
        self.keep_grant(id, &grant)?;
        // after the bind: a refused resume leaves the session on its account
        if let Some(new) = account.filter(|a| record.account.as_deref() != Some(*a)) {
            check_account(new, &record.agent)?;
            self.db().execute(
                "UPDATE sessions SET account = ?1 WHERE id = ?2",
                params![new, id],
            )?;
            record.account = Some(new.to_string());
        }
        let ws = self.open_workspace(
            record.dossier.clone(),
            &record.workdir,
            None,
            None,
            &config.workspace.root,
        )?;
        let member: Option<(String, String)> = self
            .db()
            .query_row(
                "SELECT m.thread || '/' || m.name, t.workdir FROM members m JOIN threads t ON t.id = m.thread
                 WHERE m.session = ?1 AND t.workdir IS NOT NULL",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        self.launch(
            id.to_string(),
            (&ws, bound, token),
            config,
            &Who {
                agent: &record.agent,
                command,
                account: record.account.as_deref(),
                member: member.as_ref().map(|(m, w)| (m.as_str(), Path::new(w))),
                headless,
                tools,
            },
            (mcp_json, skills.as_deref()),
            // the GPU is asked for each start: a resumed session runs without it
            (&runs, false, &grant),
        )
    }

    /// The sandbox (bwrap, `docker run` or `legionctl vm`) and agent command of session `id` working in workspace `ws`.
    fn launch(
        &self,
        id: String,
        (ws, bound, token): (&crate::workspace::Workspace, Vec<String>, &str),
        config: &Config,
        who: &Who,
        (mcp_json, skills): (serde_json::Value, Option<&[(String, PathBuf)]>),
        (runs, gpu, grant): (&Runs, bool, &crate::runnables::Grant),
    ) -> Result<Launch> {
        let runtime = PathBuf::from(
            std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?,
        );
        let manifest = &grant.manifest;
        let mcp_json = session_env(bound_mcp(mcp_json, who.tools), &id, token, &runtime);
        let [real_git, settings, mcp] =
            session_files(&runtime, &id, (config, manifest), &mcp_json)?;
        let workdir = agent_dir(ws);
        let host = matches!(runs, Runs::Host);
        // the operator's sockets read-only (a sandbox cannot put its own in their place), and of the sessions' files
        // only its own
        let legionctl = runtime.join("legionctl");
        let own = legionctl.join("sessions").join(&id);
        let mut mounts = vec![
            Mount::ReadOnly(legionctl.clone(), legionctl.clone()),
            Mount::Empty(legionctl.join("sessions")),
            Mount::ReadOnly(own.clone(), own),
        ];
        let layer = session_home(&id).join("home");
        mounts.extend(self.work_mounts(ws, config, (&layer, who.account), host)?);
        let mut lines = uses(bound, ws, &workdir);
        lines.extend(grant_mounts(
            &mut mounts,
            manifest,
            &session_home(&id).join("storage"),
            host,
        )?);
        let mut env = vec![
            ("LEGIONCTL_SESSION".to_string(), id.clone()),
            ("LEGIONCTL_TOKEN".to_string(), token.to_string()),
        ];
        let harness_args = harness_parts(manifest, &mut env);
        codex_bound_home(&mut mounts, &mut env, &id, (who.account, who.tools))?;
        team_role(&mut mounts, &mut env, &id, (who.agent, who.member))?;
        if let Some(skills) = skills {
            skill_mounts(&mut mounts, skills)?;
        }
        let gateway = (config.gateway.port != 0).then_some(config.gateway.port);
        lines.push(granted_line(grant));
        lines.push(network(&mut env, gateway));
        let mut agent: Vec<String> = who
            .command
            .iter()
            .map(|a| {
                a.replace("{id}", &id)
                    .replace("{settings}", &settings.to_string_lossy())
                    .replace("{mcp}", &mcp.to_string_lossy())
            })
            .chain(harness_args)
            .collect();
        let sandbox = match runs {
            Runs::Host => {
                headless_path(&mut env, config, who)?;
                forward(&mut agent, gateway, &legionctl.join(GATEWAY_SOCKET));
                mounts.push(Mount::ReadOnly("/usr/bin/git".into(), real_git.clone()));
                bwrap_sandbox(
                    ws,
                    (&runtime, &config.session.hidden, who.headless, gpu),
                    &mounts,
                    &env,
                )?
            }
            Runs::Container(image) => docker_sandbox(
                (&id, image, gateway, gpu),
                (&runtime, &workdir),
                mounts,
                &env,
            )?,
            Runs::Vm(disk) => vm_sandbox(
                &id,
                disk,
                &config.session.vm,
                (&runtime, gateway, gpu),
                (&workdir, mounts, &env),
            )?,
        };
        Ok(Launch {
            scope: if config.session.scope && !matches!(runs, Runs::Container(_)) {
                let unit = format!("--unit=legionctl-session-{id}");
                ["systemd-run", "--user", "--scope", "--quiet", &unit, "--"]
                    .map(String::from)
                    .to_vec()
            } else {
                Vec::new()
            },
            sandbox,
            inside: match runs {
                Runs::Host => Inside::Host,
                Runs::Container(_) => Inside::Container,
                Runs::Vm(_) => Inside::Vm,
            },
            agent,
            real_git,
            lines,
            workdir,
            mcp: mcp_json,
            tools: (!who.tools.is_empty()).then(|| who.tools.to_vec()),
            id,
        })
    }

    /// The folders of a session's work: its dossier (the evidence database read-only), its workspace (a plain
    /// folder through its overlay), what a commit writes in git, and the home layers in `layer`.
    fn work_mounts(
        &self,
        ws: &crate::workspace::Workspace,
        config: &Config,
        (layer, account): (&Path, Option<&str>),
        host: bool,
    ) -> Result<Vec<Mount>> {
        let dossier = ws.dossier.as_str();
        // its `.state` must be there for the read-only mount
        self.home(dossier)?;
        let state = self.root.join(dossier).join(".state");
        let workdir = agent_dir(ws);
        let mut mounts = vec![
            Mount::Writable(self.root.join(dossier)),
            // the dossier's database holds the test gate's evidence: only legionctl writes it
            Mount::ReadOnly(state.clone(), state),
        ];
        if ws.is_folder() {
            mounts.push(Mount::Overlay {
                folder: workdir,
                upper: crate::folder::upper(&ws.path),
                work: crate::folder::work(&ws.path),
            });
        } else {
            mounts.push(Mount::Writable(workdir.clone()));
            git_mounts(&mut mounts, &workdir, &ws.branch)?;
        }
        home_mounts(&mut mounts, config, (layer, account), host)?;
        Ok(mounts)
    }

    /// The sandbox of session `id` for the commands legionctl runs for it ([`Jail`]).
    ///
    /// # Errors
    /// No such session, its workspace is not open or is a plain folder (no repository commands), or a mount cannot
    /// be prepared.
    pub fn jail(&self, id: &str, config: &Config) -> Result<Jail> {
        let record = self.session(id)?;
        let ws = self
            .workspaces(Some(&record.dossier))?
            .into_iter()
            .find(|ws| agent_dir(ws) == record.workdir)
            .with_context(|| {
                format!(
                    "session {id} has no open workspace at {}",
                    record.workdir.display()
                )
            })?;
        anyhow::ensure!(
            !ws.is_folder(),
            "session {id} works in a plain folder, which has no repository commands"
        );
        let runtime = PathBuf::from(
            std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?,
        );
        let layer = session_home(id).join(format!("run-{}", new_id()?));
        let mounts = self.work_mounts(&ws, config, (&layer, record.account.as_deref()), true)?;
        let argv = bwrap_sandbox(
            &ws,
            (&runtime, &config.session.hidden, true, false),
            &mounts,
            &[],
        )?;
        Ok(Jail {
            argv,
            workdir: record.workdir,
            layer,
        })
    }

    /// # Errors
    /// The database read fails.
    pub fn sessions(&self) -> Result<Vec<SessionRecord>> {
        let mut stmt = self.db().prepare(
            "SELECT id, agent, dossier, workdir, started, account, inside FROM sessions ORDER BY started DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SessionRecord {
                id: r.get(0)?,
                agent: r.get(1)?,
                dossier: r.get(2)?,
                workdir: PathBuf::from(r.get::<_, String>(3)?),
                started: r.get(4)?,
                account: r.get(5)?,
                inside: Inside::from_db(&r.get::<_, String>(6)?),
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
                "SELECT id, agent, dossier, workdir, started, account, inside FROM sessions WHERE id = ?1",
                [id],
                |r| {
                    Ok(SessionRecord {
                        id: r.get(0)?,
                        agent: r.get(1)?,
                        dossier: r.get(2)?,
                        workdir: PathBuf::from(r.get::<_, String>(3)?),
                        started: r.get(4)?,
                        account: r.get(5)?,
                        inside: Inside::from_db(&r.get::<_, String>(6)?),
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
