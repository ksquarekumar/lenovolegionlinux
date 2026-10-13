//! Every legionctl operation, declared once in [`operations!`]. From that one list come:
//!
//! - [`OPS`]: name, permission class, help text and input JSON schema, from which the clients build the CLI and the
//!   MCP tools and the operator builds its HTTP routes;
//! - [`Engine`]: one method per operation, which `legionctl-operator` implements (a missing handler does not
//!   compile), and [`dispatch`], which routes a call by name;
//! - one marker type per operation in [`op`], for typed client calls, and [`render`] for the text a CLI prints.

#[cfg(feature = "client")]
pub mod client;
mod io;
mod render;
mod wire;

use std::borrow::Cow;
use std::path::PathBuf;

use anyhow::Result;
use schemars::generate::SchemaSettings;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use io::*;
pub use render::Render;
pub use wire::*;

/// Socket path relative to `$XDG_RUNTIME_DIR`.
pub const SOCKET: &str = "legionctl/api.sock";
/// The user token, relative to `$XDG_RUNTIME_DIR`: beside the socket's folder, not in it, because a session's
/// sandbox gets that folder back and must not see this file (DESIGN/03).
pub const USER_TOKEN: &str = "legionctl.token";
/// The token of a session's sandbox, in the environment of everything that runs in it.
pub const TOKEN_ENV: &str = "LEGIONCTL_TOKEN";

/// The user token in the runtime folder `runtime`, made (random, 0600) by the first client or operator that needs
/// it.
///
/// # Errors
/// The file cannot be read or made.
pub fn user_token(runtime: &std::path::Path) -> Result<String> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let path = runtime.join(USER_TOKEN);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut file) => {
            let token = random_token()?;
            file.write_all(token.as_bytes())?;
            Ok(token)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // the maker may still be writing it
            for _ in 0..50 {
                let token = std::fs::read_to_string(&path)?;
                if !token.is_empty() {
                    return Ok(token);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            anyhow::bail!("{} is empty", path.display())
        }
        Err(e) => Err(e.into()),
    }
}

/// 32 random bytes as hex.
///
/// # Errors
/// `/dev/urandom` cannot be read.
pub fn random_token() -> Result<String> {
    use std::fmt::Write as _;
    use std::io::Read as _;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    }))
}
/// HTTP header that says which surface made a call: cli, mcp or tui.
pub const ACTOR_HEADER: &str = "x-legionctl-actor";
/// The fingerprint of the operation registry a client and the operator were built with ([`api_fingerprint`]).
pub const API_HEADER: &str = "x-legionctl-api";

/// FNV-1a over every operation's name, class, input schema and output schema: equal for a client and an operator of
/// the same build, and different after a change of the wire format until the running operator restarts.
pub fn api_fingerprint() -> &'static str {
    static FINGERPRINT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    FINGERPRINT.get_or_init(|| {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for op in OPS {
            let wire = format!("{}{:?}{}{}", op.name, op.class, (op.input)(), (op.output)());
            for byte in wire.bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
            }
        }
        format!("{hash:016x}")
    })
}

/// What an operation may change, and so which gate applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    /// Reads only; no gate.
    Read,
    /// Changes only the caller's own session: its dossier, its workspace, its hooks. An agent calls these freely.
    Session,
    /// Changes the user's files and settings or other sessions; when an agent calls it, the operator asks the user
    /// first (a desktop notification, DESIGN/03).
    Workspace,
    /// Changes the machine; `legionctl-system` asks for polkit approval.
    Machine,
}

/// One operation as the surfaces see it.
#[derive(Debug, Clone)]
pub struct OpInfo {
    /// Dotted name: `workspace.open` is `POST /v1/ops/workspace.open`, `legionctl workspace open` and the MCP tool
    /// `workspace_open`.
    pub name: &'static str,
    /// The doc comment as one paragraph; see [`OpInfo::summary`].
    pub help: &'static str,
    pub class: Class,
    /// JSON schema of the input, with every subschema inline.
    pub input: fn() -> Value,
    /// JSON schema of the output, the same way.
    pub output: fn() -> Value,
}

impl OpInfo {
    /// An internal operation (`internal.*`): for legionctl's own parts (hooks, git gate, MCP proxy, `run`); not in
    /// the CLI or MCP.
    #[must_use]
    pub fn internal(&self) -> bool {
        self.name.starts_with("internal.")
    }

    /// The help text without the leading space of each doc line.
    #[must_use]
    pub fn text(&self) -> &'static str {
        self.help.trim()
    }

    /// The first sentence of the help text.
    #[must_use]
    pub fn summary(&self) -> &'static str {
        let text = self.text();
        text.split_once(". ").map_or(text, |(first, _)| first)
    }
}

/// Who made a call; the operator records it with every call that is not a read.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// The surface: cli, mcp, tui, app, hook, gate, mcp-proxy.
    pub actor: String,
    pub principal: Principal,
}

/// Whose token a call carries (DESIGN/03).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Principal {
    /// The user token: the CLI, TUI and app, and the hooks and MCP of agents that are not sandboxed.
    User,
    /// The token of a session's sandbox: the agent and everything it starts.
    Session(String),
}

/// A filesystem path in an input. Clients resolve a relative path against their own working directory before they
/// send it, because the operator runs elsewhere. Its schema has `"format": "path"`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClientPath(pub PathBuf);

impl ClientPath {
    /// The path, which must be absolute: a client resolves it against its own working directory, and a relative
    /// path (or a field an old client did not send) has no meaning in the operator.
    ///
    /// # Errors
    /// The path is not absolute.
    pub fn get(&self) -> anyhow::Result<&std::path::Path> {
        anyhow::ensure!(
            self.0.is_absolute(),
            "path '{}' is not absolute; the client must resolve it (an outdated client? restart it)",
            self.0.display()
        );
        Ok(&self.0)
    }
}

impl JsonSchema for ClientPath {
    fn schema_name() -> Cow<'static, str> {
        "ClientPath".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type": "string", "format": "path"})
    }

    fn inline_schema() -> bool {
        true
    }
}

/// The agent session that makes a call. Clients fill it from `LEGIONCTL_SESSION`, else `CLAUDE_CODE_SESSION_ID`;
/// its schema has `"format": "session"`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

impl JsonSchema for SessionId {
    fn schema_name() -> Cow<'static, str> {
        "SessionId".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type": "string", "format": "session", "description": "Agent session id; the client fills it."})
    }

    fn inline_schema() -> bool {
        true
    }
}

/// Text that a CLI reads from stdin when it is not given; its schema has `"format": "stdin"`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StdinText(pub String);

impl JsonSchema for StdinText {
    fn schema_name() -> Cow<'static, str> {
        "StdinText".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type": "string", "format": "stdin"})
    }

    fn inline_schema() -> bool {
        true
    }
}

/// A typed operation: its name and its input and output types.
pub trait Op {
    const NAME: &'static str;
    type In: Serialize + DeserializeOwned + JsonSchema;
    type Out: Serialize + DeserializeOwned + Render;
}

fn schema_of<T: JsonSchema>() -> Value {
    let schema = SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<T>();
    schema.to_value()
}

pub use legionctl_core::error::{ApiError, Code, code_of, coded};

/// A call that cannot reach an engine method.
#[derive(Debug)]
pub enum DispatchError {
    /// No operation has this name.
    Unknown(String),
    /// The input does not match the operation's schema.
    Input(serde_json::Error),
    /// The engine method failed.
    Failed(anyhow::Error),
}

impl DispatchError {
    /// The failure as the API answers it.
    #[must_use]
    pub fn api_error(&self, op: &str) -> ApiError {
        let (code, message) = match self {
            DispatchError::Unknown(name) => (Code::NotFound, format!("unknown operation {name}")),
            DispatchError::Input(e) => (Code::InvalidInput, format!("invalid input: {e}")),
            DispatchError::Failed(e) => (code_of(e), format!("{e:#}")),
        };
        ApiError {
            code,
            message,
            op: op.to_string(),
        }
    }
}

macro_rules! operations {
    ($(
        $(#[doc = $doc:literal])+
        $method:ident / $Marker:ident = $name:literal, $class:ident, $in:ty => $out:ty;
    )*) => {
        /// Every operation, in help order.
        pub const OPS: &[OpInfo] = &[$(OpInfo {
            name: $name,
            help: concat!($($doc),+),
            class: Class::$class,
            input: schema_of::<$in>,
            output: schema_of::<$out>,
        }),*];

        /// What `legionctl-operator` implements: one method per operation.
        pub trait Engine: Sync {
            $(
                $(#[doc = $doc])+
                ///
                /// # Errors
                /// The operation fails; the message goes to the caller.
                fn $method(&self, ctx: &Ctx, input: $in) -> Result<$out>;
            )*
        }

        /// Run the operation `name` with a JSON input.
        ///
        /// # Errors
        /// See [`DispatchError`].
        pub fn dispatch(engine: &dyn Engine, ctx: &Ctx, name: &str, input: Value) -> Result<Value, DispatchError> {
            match name {
                $($name => {
                    let input: $in = serde_json::from_value(input).map_err(DispatchError::Input)?;
                    let output = engine.$method(ctx, input).map_err(DispatchError::Failed)?;
                    serde_json::to_value(output).map_err(|e| DispatchError::Failed(e.into()))
                })*
                other => Err(DispatchError::Unknown(other.to_string())),
            }
        }

        /// Text for a terminal, and whether the result is a success (a CLI exits 1 when it is not).
        ///
        /// # Errors
        /// `name` is not an operation, or `output` is not its output type.
        pub fn render(name: &str, output: Value) -> Result<(String, bool)> {
            match name {
                $($name => {
                    let output: $out = serde_json::from_value(output)?;
                    Ok((output.text(), output.success()))
                })*
                other => anyhow::bail!("unknown operation {other}"),
            }
        }

        /// Marker types for typed calls: `client.call::<op::Status>(&input)`.
        pub mod op {
            use super::*;
            $(
                $(#[doc = $doc])+
                pub struct $Marker;
                impl Op for $Marker {
                    const NAME: &'static str = $name;
                    type In = $in;
                    type Out = $out;
                }
            )*
        }
    };
}

operations! {
    /// Power source, thermal mode, firmware limits, CPU policy, temperature, GPU wiring and load.
    status / Status = "status", Read, StatusIn => Snapshot;
    /// Load level (idle, normal, busy, overloaded) and why, from CPU, memory and I/O pressure. Check it before
    /// builds, test suites or parallel jobs.
    load / Load = "load", Read, Empty => LoadState;
    /// Read-only health checks: power policy drift, conflicting units, packages and modules, GPU wiring.
    doctor / Doctor = "doctor", Read, ProfileIn => Findings;
    /// Known and planned capability coverage from the profile.
    capabilities / Capabilities = "capabilities", Read, ProfileIn => CapabilityList;
    /// Check the profile's structure and ac/battery consistency (no hardware access).
    validate / Validate = "validate", Read, ProfileIn => Validation;
    /// GPU wiring: which GPU drives each display and which GPU draws the desktop.
    gpu_status / GpuStatus = "gpu.status", Read, Empty => GpuState;
    /// Environment and CPU list that run a program on a chosen GPU and core class (`legionctl run` uses it).
    gpu_run_env / GpuRunEnv = "internal.gpu.run-env", Read, RunEnvIn => RunEnv;
    /// Apply the power policy for ac, battery or the current power source (auto). A human approves it in the
    /// polkit dialog. Clears an Fn+Q choice.
    power_apply / PowerApply = "power.apply", Machine, PowerApplyIn => Changes;
    /// Choose which GPU draws the desktop (nvidia, intel, auto); Mutter reads it at the next login. A human
    /// approves it in the polkit dialog.
    gpu_primary / GpuPrimary = "gpu.primary", Machine, GpuPrimaryIn => Changes;
    /// Disable conflicting units from earlier Legion tooling and optionally unload modules; print the command that
    /// removes conflicting packages. A human approves it in the polkit dialog.
    cleanup / Cleanup = "cleanup", Machine, CleanupIn => Changes;
    /// Claude Code and Codex settings files (user and project): present, valid, which top-level sections.
    agents_list / AgentsList = "agents.list", Read, ProjectIn => ConfigFiles;
    /// Set one key of an agent's settings file (dotted: `permissions.defaultMode`, `mcp_servers.ctx.command`): the
    /// changed lines, a backup of the file, then the write; with `--dry-run` only the changed lines.
    agents_set / AgentsSet = "agents.set", Workspace, AgentSetIn => Edit;
    /// Account profiles and which agents are logged in to each.
    agents_accounts_list / AgentsAccountsList = "agents.accounts.list", Read, Empty => Vec<Account>;
    /// Make an account profile (its own Claude Code and Codex logins); `session start --account` uses it.
    agents_accounts_add / AgentsAccountsAdd = "agents.accounts.add", Workspace, AccountAddIn => Lines;
    /// Claude Code's installed plugins, each marked in or out of the allowlist (`plugins.marketplaces`).
    agents_plugins_list / AgentsPluginsList = "agents.plugins.list", Read, Empty => Vec<Plugin>;
    /// Install a Claude Code plugin that `plugins.marketplaces` allows (`claude plugin install`); a command the
    /// marketplace declares still needs a person to accept it.
    agents_plugins_install / AgentsPluginsInstall = "agents.plugins.install", Workspace, PluginInstallIn => Lines;
    /// Tokens per agent, day and model from Claude Code's and Codex's own transcripts, and Codex's rate-limit use.
    agents_usage / AgentsUsage = "agents.usage", Read, AgentUsageIn => UsageReport;
    /// Open (or reopen) this session's dossier workspace in a repository: a worktree on legionctl/<dossier> with the
    /// dossier linked as .agentworkspace. Change code there, not in the main checkout.
    workspace_open / WorkspaceOpen = "workspace.open", Session, WorkspaceOpenIn => Workspace;
    /// Git state of this session's workspaces (ahead/behind the base, staged, unstaged, untracked, conflicts) and
    /// drift from the record.
    workspace_status / WorkspaceStatusOp = "workspace.status", Read, WorkspaceIn => WorkspaceStates;
    /// Every open workspace, of every dossier.
    workspace_list / WorkspaceList = "workspace.list", Read, Empty => Workspaces;
    /// Stage everything in the workspace and commit it with the message.
    workspace_checkpoint / WorkspaceCheckpoint = "workspace.checkpoint", Session, CheckpointIn => Lines;
    /// Switch to legionctl/<dossier>--<topic>, created from the current branch the first time; without a topic, back
    /// to legionctl/<dossier>. It becomes the active branch; uncommitted changes stop it.
    workspace_branch / WorkspaceBranch = "workspace.branch", Session, BranchIn => Lines;
    /// Rebase the branch onto the base, then fast-forward the base in the main checkout (which must be clean).
    workspace_merge / WorkspaceMerge = "workspace.merge", Workspace, WorkspaceIn => Lines;
    /// Remove the worktree; the branch and the dossier stay. Refuses uncommitted changes unless `--force`.
    workspace_close / WorkspaceClose = "workspace.close", Session, CloseIn => Lines;
    /// Whether ctx and legionctl are set up for Claude Code and Codex (skills, MCP servers).
    setup_check / SetupCheck = "setup.check", Read, Empty => SetupReport;
    /// Set up ctx and legionctl for Claude Code and Codex. Changes user-level config files only, with backups.
    setup_apply / SetupApply = "setup.apply", Workspace, SetupApplyIn => Changes;
    /// Reminders that hooks inject into agent sessions, with on/off state and when they fire.
    reminders_list / RemindersList = "reminders.list", Read, ProjectIn => Reminders;
    /// Turn a reminder on.
    reminders_on / RemindersOn = "reminders.on", Workspace, IdIn => Reminders;
    /// Turn a reminder off.
    reminders_off / RemindersOff = "reminders.off", Workspace, IdIn => Reminders;
    /// Add your own reminder (one line, 300 characters max).
    reminders_add / RemindersAdd = "reminders.add", Workspace, ReminderAddIn => Reminders;
    /// Remove a reminder you added.
    reminders_rm / RemindersRm = "reminders.rm", Workspace, IdIn => Reminders;
    /// Hook actions (built-in and your commands) and what runs, in order, for each event.
    hooks_list / HooksList = "hooks.list", Read, ProjectIn => HookTable;
    /// Run an action when an event fires, after the ones already bound.
    hooks_bind / HooksBind = "hooks.bind", Workspace, HookBindIn => HookTable;
    /// Stop running an action for an event.
    hooks_unbind / HooksUnbind = "hooks.unbind", Workspace, HookBindIn => HookTable;
    /// Define your own action: a one-line shell command whose output goes to the agent.
    hooks_add / HooksAdd = "hooks.add", Workspace, HookAddIn => HookTable;
    /// Remove an action you defined, and its bindings.
    hooks_rm / HooksRm = "hooks.rm", Workspace, IdIn => HookTable;
    /// Run everything bound to an event and return the combined output, or the reason the dossier gate blocks the
    /// prompt (the agent plugin calls this through `legionctl hook`).
    hooks_run / HooksRun = "internal.hooks.run", Session, HookRunIn => HookReply;
    /// legionctl skills: where each agent has them and whether the reminder that keeps each in force is on.
    skills / Skills = "skills", Read, ProjectIn => SkillList;
    /// Telemetry: package and GPU power, temperatures, clocks, PSI, fan RPM and throttle reasons, one line per sample.
    monitor / Monitor = "monitor", Read, MonitorIn => Samples;
    /// The event log: what agents, people and the machine daemon did, oldest first.
    events / EventsOp = "events", Read, EventsIn => Page<EventRow>;
    /// Every knob legionctl reads or sets: current value, the profile's value for the current power source, the
    /// firmware range and choices, and the policy (enforce, follow, observe).
    knobs / KnobsOp = "knobs", Read, ProfileIn => Knobs;
    /// One knob and its history from the event log.
    inspect / Inspect = "inspect", Read, InspectIn => KnobDetail;
    /// The package store: each configured package, its pin, and whether its folder still has the hash recorded at
    /// fetch time.
    packages_list / PackagesList = "packages.list", Read, Empty => Packages;
    /// Fetch a package into the store (network, scratch home) and pin its content hash.
    packages_fetch / PackagesFetch = "packages.fetch", Workspace, PackageFetchIn => Manifest;
    /// Log one MCP tool call (legionctl mcp-proxy calls this for each call it forwards or refuses).
    tools_record / ToolsRecord = "internal.tools.record", Session, ToolCall => Empty;
    /// Calls through the model gateway: account, model, tokens in and out, replayed or not, status, duration.
    gateway_usage / GatewayUsageOp = "gateway.usage", Read, GatewayUsageIn => Page<GatewayUsage>;
    /// A reusable MCP tool result (legionctl mcp-proxy asks before it calls a tool its config lets it cache).
    cache_get / CacheGet = "internal.cache.get", Read, CacheGetIn => CacheValue;
    /// Keep an MCP tool result for reuse.
    cache_put / CachePut = "internal.cache.put", Session, CachePutIn => Empty;
    /// The disposable cache (`~/.cache/legionctl/cache.db`): entries and bytes of each namespace, and its limit.
    cache_stats / CacheStats = "cache.stats", Read, Empty => Vec<CacheStat>;
    /// Empty the cache, or one namespace of it (tools, gateway).
    cache_purge / CachePurge = "cache.purge", Workspace, CachePurgeIn => Lines;
    /// The long operations (session run, images build) and how each ended; the operator resumes the ones a restart
    /// cut off.
    jobs_list / JobsList = "jobs.list", Read, PageIn => Page<Job>;
    /// One job, with its result or why it failed.
    jobs_show / JobsShow = "jobs.show", Read, JobIdIn => Job;
    /// MCP tool calls of session agents through legionctl mcp-proxy, oldest first.
    tools_calls / ToolsCalls = "tools.calls", Read, ToolCallsIn => Page<ToolCallRow>;
    /// A repository's Dockerfiles as a services description (build, entrypoints, ports) and a build-cache description
    /// (base images, lockfiles); with `--write`, into the repository's `.legionctl/`.
    images_scan / ImagesScan = "images.scan", Workspace, ImagesScanIn => Scan;
    /// Build every service's image (`localhost/<repo>/<service>:<commit>`) with docker buildx.
    images_build / ImagesBuild = "images.build", Workspace, RepoIn => Lines;
    /// Write `.legionctl/compose.json` from the scan and start the services with docker compose.
    images_run / ImagesRun = "images.run", Workspace, RepoIn => Lines;
    /// The repository's compose containers: state and health.
    images_status / ImagesStatus = "images.status", Read, RepoIn => Vec<ServiceState>;
    /// Virtualization, containers and emulation: each capability with its provider and health, the containers and VMs
    /// (`WinBoat` recognized), and what is wrong.
    virt_status / VirtStatusOp = "virt.status", Read, Empty => VirtStatus;
    /// Pause, unpause, start or stop a container or VM by name (pausing `WinBoat` frees its CPU at once).
    virt_act / VirtAct = "virt.act", Workspace, VirtActIn => Lines;
    /// Pin a container or VM to the P-cores, E-cores or all CPUs and set its memory: the plan, then the change
    /// (Engine API update; virsh vcpupin and setmem), unless dry run.
    virt_tune / VirtTune = "virt.tune", Workspace, VirtTuneIn => Lines;
    /// Create a container from a local image with a core class, a memory limit and the NVIDIA GPUs: the plan, then
    /// the container (not started), unless dry run.
    virt_create / VirtCreate = "virt.create", Workspace, VirtCreateIn => Lines;
    /// Dual boot: the Windows boot entry, a pending one-time boot, NTFS volumes Windows left dirty, and what to fix.
    windows_status / WindowsStatus = "windows.status", Read, Empty => WindowsState;
    /// Restart into Windows once (the start after returns to Linux). A human approves it in the polkit dialog.
    windows_reboot / WindowsReboot = "windows.reboot", Machine, DryRunIn => Changes;
    /// Record this machine into a directory: the files legionctl reads, the profile, the Fn+Q ledger and recent
    /// events. A bug report, a test fixture, or a before/after record of a BIOS or kernel update.
    record / Record = "record", Workspace, RecordIn => Recording;
    /// What the daemon would write and what doctor finds on a recorded machine, with its events.
    replay / Replay = "replay", Read, ReplayIn => Recording;
    /// Run a workload (stress-ng, gpu-burn or both) with telemetry each second; keeps the scores, a summary and the
    /// knob values in the event log. Loads the machine fully for the duration.
    bench_run / BenchRunOp = "bench.run", Workspace, BenchIn => BenchRun;
    /// Bench runs, oldest first.
    bench_list / BenchList = "bench.list", Read, PageIn => Page<BenchRun>;
    /// Step a firmware power limit through values with a bench workload at each, on AC and an idle machine; returns
    /// each step and the recommended value (the knee). The original value comes back. A human approves it in the
    /// polkit dialog.
    tune / Tune = "tune", Machine, TuneIn => TuneReport;
    /// Two bench runs side by side: scores, telemetry and the knobs that differ.
    bench_compare / BenchCompareOp = "bench.compare", Read, BenchCompareIn => BenchCompare;
    /// Bind this session to a dossier (one writable dossier per session); a new name makes it from the pattern.
    dossier_use / DossierUse = "dossier.use", Session, DossierUseIn => Lines;
    /// This session's active dossier.
    dossier_active / DossierActive = "dossier.active", Read, SessionIn => Lines;
    /// Give up this session's dossier.
    dossier_release / DossierRelease = "dossier.release", Session, SessionIn => Lines;
    /// Which session holds which dossier, and stale bindings.
    dossier_locks / DossierLocks = "dossier.locks", Read, Empty => Locks;
    /// Dossiers, most recently changed first.
    dossier_list / DossierListOp = "dossier.list", Read, Empty => DossierList;
    /// Read a dossier file; each line prints as <line>:<hash>|<text>, the anchors that edit takes.
    dossier_read / DossierRead = "dossier.read", Read, DossierReadIn => Lines;
    /// Search every dossier (or one) for a text; rows are <path>:<line>:<hash>|<text>.
    dossier_search / DossierSearch = "dossier.search", Read, DossierSearchIn => Lines;
    /// Write a new file in this session's dossier (content from stdin); `--overwrite` replaces an existing one.
    dossier_write / DossierWrite = "dossier.write", Session, DossierWriteIn => Lines;
    /// Append to a file in this session's dossier (content from stdin).
    dossier_append / DossierAppend = "dossier.append", Session, DossierAppendIn => Lines;
    /// Change anchored lines of a file in this session's dossier; an anchor from an outdated read is refused.
    dossier_edit / DossierEdit = "dossier.edit", Session, DossierEditIn => Lines;
    /// Create the next numbered artifact <CLASS>/NN-<slug>.md in this session's dossier (content from stdin).
    dossier_add / DossierAdd = "dossier.add", Session, DossierAddIn => Lines;
    /// Ledger findings for a dossier (default: this session's): a missing plan.md or plan section.
    dossier_check / DossierCheck = "dossier.check", Read, DossierCheckIn => DossierFindings;
    /// The dossier's work graph: plan rows and artifacts, linked by supports, supersedes, code and plan references.
    dossier_graph / DossierGraph = "dossier.graph", Read, DossierGraphIn => Graph;
    /// The dossier nudges in order: each level a session reached (info, warn, block) with its count of replies that
    /// changed the workspace, and each answer (a dossier write).
    dossier_nudges / DossierNudges = "dossier.nudges", Read, DossierNudgesIn => Page<EventRow>;
    /// Whether this session may write a path: inside the dossier root only its active dossier (agent hooks use it).
    dossier_guard / DossierGuard = "internal.dossier.guard", Read, GuardIn => Guard;
    /// Prepare a sandboxed session of an agent on a dossier's workspace; `legionctl session start` then runs it in the
    /// terminal (systemd scope, bwrap with your home read-only, git gate).
    session_start / SessionStart = "session.start", Workspace, SessionStartIn => Launch;
    /// Run an agent headless on a task through its ACP adapter, in the same sandbox, gates and MCP servers as an
    /// interactive session; returns its answer.
    session_run / SessionRun = "session.run", Workspace, SessionRunIn => RunOutput;
    /// Sessions legionctl started, newest first, with the state of their scope.
    session_list / SessionList = "session.list", Read, Empty => Sessions;
    /// Stop a session's scope (the agent and everything it started).
    session_stop / SessionStop = "session.stop", Workspace, SessionIdIn => Lines;
    /// The programs a session's processes started, oldest first (legionctl-system's eBPF sensor; needs its scope).
    session_activity / SessionActivity = "session.activity", Read, SessionActivityIn => Page<SessionExec>;
    /// Freeze a session's processes (cgroup freezer): memory stays, the agent does nothing until `session unpause`.
    session_pause / SessionPause = "session.pause", Workspace, SessionIdIn => Lines;
    /// Let a paused session run again.
    session_unpause / SessionUnpause = "session.unpause", Workspace, SessionIdIn => Lines;
    /// Continue an ended session: the same id, dossier and workspace, and the agent's resume command.
    session_resume / SessionResume = "session.resume", Workspace, SessionResumeIn => Launch;
    /// Whether a git command may run in a session sandbox (the git gate asks this before every git call).
    gates_check / GatesCheck = "internal.gates.check", Read, GateIn => Guard;
    /// The merged config (defaults, /etc/legionctl, ~/.config/legionctl, the repository's .legionctl) and the files
    /// or keys that were not used.
    config_show / ConfigShow = "config.show", Read, ProjectIn => ConfigView;
    /// Which file set a key, the values it overrode, and whether /etc/legionctl locks it.
    config_explain / ConfigExplain = "config.explain", Read, ExplainIn => Explanation;
    /// The config layers in merge order, their files, and the locked keys.
    config_layers / ConfigLayers = "config.layers", Read, ProjectIn => LayerList;
}
