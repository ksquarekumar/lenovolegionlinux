//! Inputs and outputs of the operations. A field's doc comment is its help text on every surface.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::wire::{
    Action, BenchResult, Binding, Capability, ConfigFile, ConfigLayer, DesktopGpu, DossierInfo,
    EditMode, Event, EventRow, GpuMode, Knob, Lock, Output, PackageState, Reminder, Sample,
    Scenario, SessionRecord, SkillInfo, Workspace, WorkspaceStatus,
};
use crate::{ClientPath, SessionId, StdinText};

pub type ConfigFiles = Vec<ConfigFile>;
pub type Workspaces = Vec<Workspace>;
pub type WorkspaceStates = Vec<WorkspaceStatus>;
pub type Reminders = Vec<Reminder>;
pub type SkillList = Vec<SkillInfo>;
pub type Locks = Vec<Lock>;
pub type DossierList = Vec<DossierInfo>;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Empty {}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ProfileIn {
    /// Profile (JSONC); default: the installed one.
    pub profile: Option<ClientPath>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct StatusIn {
    /// Profile (JSONC); default: the installed one.
    pub profile: Option<ClientPath>,
    /// Read sysfs and procfs only (for views that refresh every second); skip systemd, nvidia-smi and firmware calls.
    pub fast: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProjectIn {
    /// Project directory: its agent settings and its repository's .legionctl config layer apply.
    #[serde(default = "here")]
    pub project: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExplainIn {
    /// Dotted key, such as hooks.bindings.prompt or load.busy.
    pub key: String,
    /// Project directory; its repository's .legionctl layer applies.
    #[serde(default = "here")]
    pub project: ClientPath,
}

/// The merged config and what was not used.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConfigView {
    pub value: serde_json::Value,
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LayerList {
    /// Lowest first.
    pub layers: Vec<ConfigLayer>,
    pub locked: Vec<String>,
    pub problems: Vec<String>,
}

fn here() -> ClientPath {
    ClientPath(".".into())
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceOpenIn {
    /// A folder in the repository (its main checkout or any worktree of it).
    #[serde(default = "here")]
    pub repo: ClientPath,
    /// Ref the dossier's branch starts from and merges into; default: the main checkout's current branch.
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub session: SessionId,
}

/// Which of this session's workspaces: the one in `repo`, or the only one.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceIn {
    /// A folder in the repository; needed only when the dossier has workspaces in several repositories.
    #[serde(default)]
    pub repo: Option<ClientPath>,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CheckpointIn {
    /// Commit message: what this change set does (from the plan entry).
    pub message: String,
    /// A folder in the repository; needed only when the dossier has workspaces in several repositories.
    #[serde(default)]
    pub repo: Option<ClientPath>,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BranchIn {
    /// The branch becomes legionctl/<dossier>--<topic> (created the first time); without it, legionctl/<dossier>.
    #[serde(default)]
    pub topic: Option<String>,
    /// A folder in the repository; needed only when the dossier has workspaces in several repositories.
    #[serde(default)]
    pub repo: Option<ClientPath>,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct CloseIn {
    /// Discard uncommitted changes.
    #[serde(default)]
    pub force: bool,
    /// A folder in the repository; needed only when the dossier has workspaces in several repositories.
    #[serde(default)]
    pub repo: Option<ClientPath>,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SetupApplyIn {
    /// Install ctx with its signed installer if it is missing.
    pub install_ctx: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AccountAddIn {
    /// Profile name: a-z, 0-9 and '-'.
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IdIn {
    /// The reminder's or hook action's id (`reminders list`, `hooks list`).
    pub id: String,
    /// Project directory; its repository's .legionctl layer applies.
    #[serde(default = "here")]
    pub project: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReminderAddIn {
    /// a-z, 0-9 and '-', 32 characters max.
    pub id: String,
    /// One line, 300 characters max.
    pub text: String,
    /// When to inject it; prompt fires on every turn, so keep those short.
    #[serde(default = "session_start")]
    pub event: Event,
    /// Project directory; its repository's .legionctl layer applies.
    #[serde(default = "here")]
    pub project: ClientPath,
}

fn session_start() -> Event {
    Event::SessionStart
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HookBindIn {
    /// Hook event: session-start, prompt or stop.
    pub event: Event,
    /// Action id: a built-in (reminders, load, load-if-busy, setup-check) or one you defined.
    pub action: String,
    /// Project directory; its repository's .legionctl layer applies.
    #[serde(default = "here")]
    pub project: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HookAddIn {
    /// Action id: a-z, 0-9 and '-'.
    pub id: String,
    /// One-line shell command; it runs as you with `sh -c`, and its stdout goes to the agent.
    pub command: String,
    /// Seconds before it is stopped (1-30); the agent waits for hooks.
    #[serde(default = "five")]
    pub timeout_s: u32,
    /// Project directory; its repository's .legionctl layer applies.
    #[serde(default = "here")]
    pub project: ClientPath,
}

fn five() -> u32 {
    5
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PowerSource {
    #[default]
    Auto,
    Ac,
    Battery,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PowerApplyIn {
    /// Which policy: auto is the current power source.
    pub source: PowerSource,
    /// Print what would change; change nothing.
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GpuPrimaryIn {
    /// The GPU that draws the desktop: nvidia, intel, or auto (Mutter decides).
    pub choice: DesktopGpu,
    /// Print what would change; change nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct CleanupIn {
    /// Also unload conflicting kernel modules.
    pub unload_modules: bool,
    /// Print what would change; change nothing.
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RunGpu {
    Nvidia,
    Intel,
    #[default]
    Default,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Cores {
    P,
    E,
    #[default]
    All,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RunEnvIn {
    /// GPU that renders the program.
    pub gpu: RunGpu,
    /// Core class: p (performance), e (efficiency) or all.
    pub cores: Cores,
}

/// Lines that say what changed or, in a dry run, what would change.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Changes {
    pub lines: Vec<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Findings {
    /// Empty when nothing is wrong.
    pub findings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CapabilityList {
    pub profile_id: String,
    pub capabilities: Vec<Capability>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Validation {
    pub path: String,
    /// Empty when the profile is valid.
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GpuState {
    pub mode: GpuMode,
    /// GPU GNOME draws the desktop on; Mutter reads it at login.
    pub desktop_gpu: DesktopGpu,
    pub outputs: Vec<Output>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunEnv {
    pub env: Vec<(String, String)>,
    /// CPU list for `AllowedCPUs=`; none for all cores.
    pub cpus: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HookTable {
    pub actions: Vec<Action>,
    /// In the order they run.
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HookRunIn {
    /// Hook event: session-start, prompt or stop.
    pub event: Event,
    /// Project directory; its repository's .legionctl layer applies.
    #[serde(default = "here")]
    pub project: ClientPath,
    /// The agent session; without one the dossier gate does not apply.
    #[serde(default)]
    pub session: Option<SessionId>,
    /// The prompt text (prompt event): a prompt that runs /legionctl:dossier passes the gate.
    #[serde(default)]
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct HookReply {
    pub text: String,
    /// Set when the prompt is blocked: the reason the user sees.
    pub block: Option<String>,
}

/// Lines of output (dossier rows, messages).
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Lines {
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SessionIn {
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DossierUseIn {
    /// Dossier name: a-z, 0-9 and '-'. A name that does not exist makes a new dossier from the pattern.
    pub name: String,
    /// Take it over from another live session; for an agent, the operator asks the user first.
    #[serde(default)]
    #[schemars(extend("x-approval" = true))]
    pub steal: bool,
    #[serde(default)]
    pub session: SessionId,
    /// A folder in the repository the session works in; the next session there gets this dossier if it is free.
    #[serde(default = "here")]
    pub repo: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DossierReadIn {
    /// <dossier>/<file>, relative to the dossier root.
    pub path: String,
    /// First line (1-based).
    #[serde(default = "one")]
    pub first: u32,
    /// Last line; default: the end.
    #[serde(default)]
    pub last: Option<u32>,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DossierSearchIn {
    /// Text to find (not a regular expression).
    pub pattern: String,
    /// Only this dossier.
    #[serde(default)]
    pub dossier: Option<String>,
    /// Match without regard to upper and lower case.
    #[serde(default)]
    pub ignore_case: bool,
    /// At most this many lines.
    #[serde(default = "two_hundred")]
    pub limit: u32,
}

fn two_hundred() -> u32 {
    200
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DossierWriteIn {
    /// <dossier>/<file>, relative to the dossier root.
    pub path: String,
    /// The file's text; default: stdin.
    pub content: StdinText,
    /// Replace an existing file.
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DossierAppendIn {
    /// <dossier>/<file>, relative to the dossier root.
    pub path: String,
    /// The text to append; default: stdin.
    pub content: StdinText,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DossierEditIn {
    /// <dossier>/<file>, relative to the dossier root.
    pub path: String,
    /// replace (the lines from start to end), insert-after (after start) or delete.
    pub mode: EditMode,
    /// Anchor <line>:<hash> from a read.
    pub start: String,
    /// End anchor for replace and delete; default: the start line.
    #[serde(default)]
    pub end: Option<String>,
    /// New lines for replace and insert-after.
    #[serde(default)]
    pub content: StdinText,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DossierAddIn {
    /// Artifact class, optionally with a subfolder: MEMORY, DESIGN, EVIDENCE/validation, ...
    pub class: String,
    /// a-z, 0-9 and '-'.
    pub slug: String,
    /// The artifact's text; default: stdin.
    pub content: StdinText,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct DossierGraphIn {
    /// Dossier; default: this session's.
    #[serde(default)]
    pub dossier: Option<String>,
    /// Only this node and its neighbours (`plan#todo-3`, `MEMORY/03-x`).
    #[serde(default)]
    pub node: Option<String>,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct DossierNudgesIn {
    /// Only this session (id from `session list` or `dossier locks`); default: every session.
    #[serde(default)]
    pub of: Option<String>,
    /// Where the page starts: the `next` of the page before; default: the oldest.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Items on the page (1-1000).
    #[serde(default = "two_hundred")]
    pub limit: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct DossierCheckIn {
    /// Dossier to check; default: this session's.
    #[serde(default)]
    pub dossier: Option<String>,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DossierFindings {
    pub dossier: String,
    /// Empty when the ledger is complete.
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GuardIn {
    /// Absolute path that a tool is about to write.
    pub path: String,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Guard {
    /// Set when the write is refused.
    pub refusal: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionStartIn {
    /// Agent to start: claude or codex (config session.agents).
    pub agent: String,
    /// Dossier to work in (a new name makes a new dossier); default: the last one used in this repository, if no
    /// live session holds it.
    #[serde(default)]
    pub dossier: Option<String>,
    /// Take the dossier over from another live session.
    #[serde(default)]
    pub steal: bool,
    /// A folder in the repository to work on.
    #[serde(default = "here")]
    pub repo: ClientPath,
    /// An account profile (`agents accounts add`): its logins and transcripts instead of `~/.claude` and `~/.codex`.
    #[serde(default)]
    pub account: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionRunIn {
    /// Agent: claude or codex (config session.acp: its ACP adapter).
    pub agent: String,
    /// The task for the agent.
    pub prompt: String,
    /// Dossier to work in (a new name makes a new dossier); default: the last one used in this repository, if no
    /// live session holds it.
    #[serde(default)]
    pub dossier: Option<String>,
    /// Take the dossier over from another live session.
    #[serde(default)]
    pub steal: bool,
    /// A folder in the repository to work on.
    #[serde(default = "here")]
    pub repo: ClientPath,
    /// An account profile (`agents accounts add`): its logins and transcripts instead of `~/.claude` and `~/.codex`.
    #[serde(default)]
    pub account: Option<String>,
}

/// What a headless run gave back.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunOutput {
    pub id: String,
    /// The agent's answer.
    pub text: String,
    pub ms: u64,
    /// Each permission the agent asked for, and the answer (the sandbox and the gates are the boundary, so asks
    /// inside it are allowed).
    pub permissions: Vec<String>,
}

/// A session and whether its scope still runs.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionState {
    #[serde(flatten)]
    pub session: SessionRecord,
    /// The scope's state from systemd: active, inactive, failed, ...
    pub state: String,
}

pub type Sessions = Vec<SessionState>;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionIdIn {
    /// Session id (`session list`).
    pub id: String,
}

/// A program that a process in an agent session started (the eBPF sensor in legionctl-system).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionExec {
    /// Seconds since the epoch.
    pub at: i64,
    pub pid: u32,
    /// The arguments, cut at 512 bytes.
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionResumeIn {
    /// Session id (`session list`).
    pub id: String,
    /// Continue on this account profile instead (its rate limits, its login); Claude Code's transcript is copied
    /// there, so the conversation goes on.
    pub account: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GateIn {
    /// The git arguments, as the command got them.
    pub args: Vec<String>,
    /// Where the command runs; its repository's config layer and aliases apply.
    #[serde(default = "here")]
    pub project: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MonitorIn {
    /// Samples to take (1-3600).
    #[serde(default = "one")]
    pub count: u32,
    /// Window of each sample in milliseconds (100-60000); package power is the mean over it.
    #[serde(default = "thousand")]
    pub interval_ms: u32,
}

fn thousand() -> u32 {
    1000
}

pub type Samples = Vec<Sample>;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EventsIn {
    /// Where the page starts: the `next` of the page before; default: the oldest.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Items on the page (1-1000).
    #[serde(default = "two_hundred")]
    pub limit: u32,
}

pub type Events = Vec<EventRow>;

/// One page of a log, oldest first.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// The cursor of the next page; none on the last one.
    pub next: Option<String>,
}

pub type Knobs = Vec<Knob>;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InspectIn {
    /// Knob id (`cpu.epp`, `firmware.ppt_pl1_spl`, ...) or its path.
    pub knob: String,
    /// Profile (JSONC); default: the installed one.
    pub profile: Option<ClientPath>,
    /// Where the history page starts: the `next` of the page before; default: the oldest.
    #[serde(default)]
    pub cursor: Option<String>,
    /// History events on the page (1-1000).
    #[serde(default = "two_hundred")]
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RecordIn {
    /// New (or empty) directory for the recording.
    pub dir: ClientPath,
    /// Events of the last minutes to keep.
    #[serde(default = "ten")]
    pub minutes: u32,
}

fn ten() -> u32 {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReplayIn {
    /// A directory that `legionctl record` wrote.
    pub dir: ClientPath,
}

/// What the daemon and `doctor` decide on a machine: live for `record`, on the recorded tree for `replay`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Recording {
    pub dir: String,
    /// Machine paths that `record` wrote (0 for `replay`).
    pub paths: usize,
    /// What `power apply` (auto) would write.
    pub plan: Vec<String>,
    /// The machine checks of `doctor`.
    pub findings: Vec<String>,
    pub knobs: Knobs,
    pub events: Events,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BenchIn {
    /// cpu: stress-ng on every CPU; gpu: gpu-burn on the NVIDIA GPU; mixed: both at once.
    pub scenario: Scenario,
    /// Duration (1-600).
    #[serde(default = "thirty")]
    pub seconds: u32,
    /// A name for the run (`pl1-90`, `scx-lavd`).
    pub label: Option<String>,
}

fn thirty() -> u32 {
    30
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BenchRun {
    /// The event id of the run; `bench compare` takes two.
    pub id: i64,
    /// Seconds since the epoch.
    pub at: i64,
    #[serde(flatten)]
    pub result: BenchResult,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BenchCompareIn {
    /// The earlier run (its id from `bench list`).
    pub earlier: i64,
    /// The later run.
    pub later: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BenchCompare {
    pub a: BenchRun,
    pub b: BenchRun,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TuneIn {
    /// Firmware limit to step: `ppt_pl1_spl`, `ppt_pl2_sppt`, `ppt_cpu_cl`, `cpu_temp` or `gpu_nv_ac_offset`.
    pub attribute: String,
    /// Values to step through (W, or °C for `cpu_temp`), each within the firmware range.
    pub values: Vec<u32>,
    /// The workload of each step: cpu, gpu or mixed.
    pub scenario: Scenario,
    /// Duration of each step (1-600).
    #[serde(default = "sixty")]
    pub seconds: u32,
    /// Pause between steps (0-600).
    #[serde(default = "thirty")]
    pub cooldown_s: u32,
    /// Run also when the machine is not idle (the scores then mix in other work).
    #[serde(default)]
    pub force: bool,
}

fn sixty() -> u32 {
    60
}

pub type Packages = Vec<PackageState>;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PackageFetchIn {
    /// A package from the config (`packages`).
    pub name: String,
    /// Fetch again even when this version is in the store (after `packages list` shows it changed).
    #[serde(default)]
    pub force: bool,
}

/// One MCP tool call through `legionctl mcp-proxy`, as the event log keeps it (no arguments, only their hash).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolCall {
    /// The agent session (`LEGIONCTL_SESSION`); a sandbox's own, whatever it sends.
    #[serde(default)]
    pub session: SessionId,
    pub server: String,
    pub tool: String,
    /// BLAKE2b-128 of the arguments' JSON, hex.
    pub args_hash: String,
    pub ms: u64,
    /// Size of the result's JSON.
    pub bytes: u64,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CacheGetIn {
    /// The tool call the result belongs to; the operator builds the key from it.
    #[serde(flatten)]
    pub call: CachedCall,
}

/// A tool call as the cache knows it (DESIGN/03): the operator builds the key from these, from the git state of
/// `workspace`, and from the caller's session, so one sandbox cannot put entries under another's keys.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CachedCall {
    /// The server: its command line (for a package, it names the store version).
    pub server: String,
    pub tool: String,
    /// The call's arguments.
    pub args: serde_json::Value,
    /// The folder whose git state the result depends on, for a tool that reads the working copy.
    #[serde(default)]
    pub workspace: Option<ClientPath>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CachePutIn {
    #[serde(flatten)]
    pub call: CachedCall,
    /// The tool result as JSON.
    pub value: String,
    /// Seconds the entry stays valid.
    pub ttl_s: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct CachePurgeIn {
    /// Only this namespace (tools, gateway); default: all.
    #[serde(default)]
    pub ns: Option<String>,
}

/// One namespace of the disposable cache.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CacheStat {
    pub ns: String,
    pub entries: u64,
    pub bytes: u64,
    /// The namespace's limit (`cache.<ns>.max_bytes`).
    pub max_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct CacheValue {
    /// None: not cached or expired.
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImagesScanIn {
    /// The repository.
    #[serde(default = "here")]
    pub repo: ClientPath,
    /// Write `.legionctl/docker-compose-services.json` and `docker-compose-cache.json` in the repository.
    #[serde(default)]
    pub write: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum VirtAction {
    /// Freeze it: CPU free at once, memory kept.
    Pause,
    Unpause,
    Start,
    /// A clean stop (a VM gets an ACPI shutdown).
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VirtActIn {
    /// start, stop, pause or unpause.
    pub action: VirtAction,
    /// A container or VM name from `virt status`.
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CoreClass {
    /// The performance cores.
    P,
    /// The efficiency cores.
    E,
    /// Every online CPU.
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VirtTuneIn {
    /// A container or VM name from `virt status`.
    pub name: String,
    /// The cores it may run on.
    pub cores: Option<CoreClass>,
    /// Its memory limit (a container) or balloon size (a VM): bytes, or with k, m or g (powers of 1024).
    pub memory: Option<String>,
    /// Show the plan; change nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VirtCreateIn {
    /// The new container's name.
    pub name: String,
    /// A local image (`docker images`); legionctl does not pull.
    pub image: String,
    /// Pin the container to the P-cores, the E-cores, or all CPUs.
    pub cores: Option<CoreClass>,
    /// Its memory limit: bytes, or with k, m or g (powers of 1024).
    pub memory: Option<String>,
    /// All NVIDIA GPUs through the NVIDIA runtime (`virt status`: GPU compute in containers).
    #[serde(default)]
    pub gpu: bool,
    /// Show the plan; create nothing.
    #[serde(default)]
    pub dry_run: bool,
}

/// A compose service's container and its health.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ServiceState {
    pub service: String,
    pub container: String,
    /// created, running, exited, ...
    pub state: String,
    /// healthy, unhealthy or starting, when the service has a health check.
    pub health: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RepoIn {
    /// The repository.
    #[serde(default = "here")]
    pub repo: ClientPath,
}

/// One capability of the machine (note 17) and the provider that gives it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VirtCapability {
    pub name: String,
    pub provider: String,
    pub up: bool,
    pub detail: String,
}

/// How a proxied tool call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Ok,
    /// An earlier result was reused.
    Cached,
    /// The tool reported an error.
    Error,
    /// The server did not answer.
    Failed,
    /// The tool is not allowed.
    Refused,
}

/// A container or a virtual machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum VirtKind {
    Container,
    Vm,
}

/// A container or VM.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VirtInstance {
    pub kind: VirtKind,
    pub name: String,
    pub image: String,
    pub state: String,
    /// The app it belongs to, when legionctl recognizes it (`WinBoat` by its image).
    pub consumer: Option<String>,
    /// Host memory it holds while it runs: a container's use without its page cache, a VM's resident memory.
    pub memory_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VirtStatus {
    pub capabilities: Vec<VirtCapability>,
    pub instances: Vec<VirtInstance>,
    /// What is wrong, with the fix in words.
    pub findings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AgentName {
    Claude,
    Codex,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SettingsScope {
    /// `~/.claude/settings.json`, `~/.codex/config.toml`.
    User,
    /// `<project>/.claude/settings.json`, `<project>/.codex/config.toml`.
    Project,
    /// `<project>/.claude/settings.local.json` (Claude Code only).
    ProjectLocal,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentSetIn {
    /// claude or codex.
    pub agent: AgentName,
    /// Which settings file: user, project or project-local.
    pub scope: SettingsScope,
    /// Dotted key: `permissions.defaultMode`, `mcp_servers.ctx.command`.
    pub key: String,
    /// JSON (`true`, `3`, `["a"]`, `{"k": 1}`) or plain text. Not kept in the audit: it can be a token.
    #[schemars(extend("x-secret" = true))]
    pub value: String,
    /// The project whose settings files project and project-local mean.
    #[serde(default = "here")]
    pub project: ClientPath,
    /// Show the changed lines; write nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct AgentUsageIn {
    /// Transcripts changed in the last days.
    #[serde(default = "seven")]
    pub days: u64,
    /// An account profile's transcripts instead of `~/.claude` and `~/.codex`.
    #[serde(default)]
    pub account: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PluginInstallIn {
    /// `<plugin>@<marketplace>`, both allowed in `plugins.marketplaces`.
    pub id: String,
    /// Where Claude Code records the plugin: user, project or project-local.
    pub scope: SettingsScope,
    /// The project for the project scopes.
    #[serde(default = "here")]
    pub project: ClientPath,
}

/// A Claude Code plugin as `claude plugin list` reports it, and whether the allowlist has it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Plugin {
    /// `<plugin>@<marketplace>`.
    pub id: String,
    pub version: String,
    pub scope: String,
    pub enabled: bool,
    pub allowed: bool,
}

/// An account profile: its own Claude Code and Codex logins and transcripts.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Account {
    pub name: String,
    pub dir: String,
    /// Claude Code is logged in there.
    pub claude: bool,
    /// Codex is logged in there.
    pub codex: bool,
}

fn seven() -> u64 {
    7
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct GatewayUsageIn {
    /// Where the page starts: the `next` of the page before; default: the oldest.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Items on the page (1-1000).
    #[serde(default = "two_hundred")]
    pub limit: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ToolCallsIn {
    /// Only this session's calls (id from `session list`).
    pub session: Option<String>,
    /// Where the page starts: the `next` of the page before; default: the oldest.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Items on the page (1-1000).
    #[serde(default = "two_hundred")]
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolCallRow {
    pub id: i64,
    pub at: i64,
    #[serde(flatten)]
    pub call: ToolCall,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct DryRunIn {
    /// Show what would happen; change nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KnobDetail {
    pub knob: Knob,
    /// Events that name the knob's file (writes, warnings), oldest first.
    pub history: Page<EventRow>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SessionActivityIn {
    /// Session id (`session list`).
    pub id: String,
    /// Where the page starts: the `next` of the page before; default: the oldest.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Items on the page (1-1000).
    #[serde(default = "two_hundred")]
    pub limit: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PageIn {
    /// Where the page starts: the `next` of the page before; default: the oldest.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Items on the page (1-1000).
    #[serde(default = "two_hundred")]
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JobIdIn {
    /// Job id (`jobs list`).
    pub id: i64,
}
