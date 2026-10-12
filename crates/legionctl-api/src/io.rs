//! Inputs and outputs of the operations. A field's doc comment is its help text on every surface.

pub use legionctl_core::reminders::Event;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ClientPath, SessionId, StdinText};
pub use legionctl_core::dossier::{DossierInfo, EditMode, Lock};

pub use legionctl_core::agents::ConfigFile;
pub use legionctl_core::config::{Explanation, Layer as ConfigLayer};
pub use legionctl_core::hooks::{Action, Binding};
pub use legionctl_core::reminders::Reminder;
pub use legionctl_core::setup::Report as SetupReport;
pub use legionctl_core::skills::SkillInfo;
pub use legionctl_core::worktree::{Worktree, WorktreeStatus};
pub use legionctl_machine::gpu::Output;
pub use legionctl_machine::load::Load as LoadState;
pub use legionctl_machine::profile::Capability;
pub use legionctl_machine::snapshot::Snapshot;

pub type ConfigFiles = Vec<ConfigFile>;
pub type Worktrees = Vec<WorktreeStatus>;
pub type Reminders = Vec<Reminder>;
pub type SkillList = Vec<SkillInfo>;
pub type Locks = Vec<Lock>;
pub type Dossiers = Vec<DossierInfo>;

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

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct WorktreeListIn {
    /// Measure the disk usage of each worktree (slower on large checkouts).
    pub sizes: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorktreeCreateIn {
    /// Worktree name: letters, digits, '-', '_', '.'; the branch is legionctl/NAME.
    pub name: String,
    /// Git repository to branch from.
    #[serde(default = "here")]
    pub repo: ClientPath,
    /// Git ref to start from; default: the repository's HEAD.
    #[serde(default)]
    pub base: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorktreeRmIn {
    pub name: String,
    /// Discard uncommitted changes.
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SetupApplyIn {
    /// Install ctx with its signed installer if it is missing.
    pub install_ctx: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IdIn {
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
pub struct EventIn {
    pub event: Event,
    /// Project directory; its repository's .legionctl layer applies.
    #[serde(default = "here")]
    pub project: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HookBindIn {
    pub event: Event,
    /// Action id: a built-in (reminders, load, load-if-busy, setup-check) or one you defined.
    pub action: String,
    /// Project directory; its repository's .legionctl layer applies.
    #[serde(default = "here")]
    pub project: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HookAddIn {
    pub id: String,
    /// One-line shell command; it runs as you with `sh -c`, and its stdout goes to the agent.
    pub command: String,
    /// Seconds before it is stopped (1-30); the agent waits for hooks.
    #[serde(default = "five")]
    pub timeout: u32,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DesktopGpu {
    Nvidia,
    Intel,
    Auto,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GpuPrimaryIn {
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
    /// hybrid (panel on the iGPU), discrete (panel on NVIDIA) or unknown.
    pub mode: String,
    /// GPU GNOME draws the desktop on (nvidia, intel or auto); Mutter reads it at login.
    pub desktop_gpu: String,
    pub outputs: Vec<Output>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunEnv {
    pub env: Vec<(String, String)>,
    /// CPU list for `AllowedCPUs=`; none for all cores.
    pub cpus: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Text {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HookTable {
    pub actions: Vec<Action>,
    /// In the order they run.
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HookRunIn {
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
    /// Dossier name: a-z, 0-9 and '-'.
    pub name: String,
    /// Create it from the pattern when it does not exist.
    #[serde(default)]
    pub create: bool,
    /// Take it over from another live session (with the user's agreement).
    #[serde(default)]
    pub steal: bool,
    #[serde(default)]
    pub session: SessionId,
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
    #[serde(default)]
    pub ignore_case: bool,
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
    pub content: StdinText,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DossierEditIn {
    /// <dossier>/<file>, relative to the dossier root.
    pub path: String,
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
    pub content: StdinText,
    #[serde(default)]
    pub session: SessionId,
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
