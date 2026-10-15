//! Inputs and outputs of the operations. A field's doc comment is its help text on every surface.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::wire::{
    Action, Battery, BenchResult, Binding, Capability, ConfigFile, ConfigLayer, DesktopGpu,
    DossierInfo, EditMode, Event, EventRow, FirmwareDevice, GpuMode, Harness, Inside, Knob, Lock,
    NvidiaPower, Output, PackageState, Reading, Reminder, RuleLayer, Sample, Scenario,
    SessionRecord, SkillInfo, VfioDevice, ViewWidget, Workspace,
};
use crate::{ClientPath, SessionId, StdinText};

pub type ConfigFiles = Vec<ConfigFile>;
pub type Workspaces = Vec<Workspace>;
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

fn on_host() -> Inside {
    Inside::Host
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
    /// A separate workspace of the dossier for this topic, on legionctl/<dossier>--<topic> (created the first time,
    /// reused after): one per agent that works in parallel.
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub session: SessionId,
}

/// Which of this session's workspaces: the topic's, else the one that contains `repo`, else the dossier's own
/// workspace in the repository of `repo`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceIn {
    /// A folder in a workspace or in the repository.
    #[serde(default = "here")]
    pub repo: ClientPath,
    /// The topic workspace (`workspace open --topic`), from anywhere in the repository.
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CheckpointIn {
    /// Commit message: what this change set does (from the plan entry).
    pub message: String,
    /// A folder in a workspace or in the repository.
    #[serde(default = "here")]
    pub repo: ClientPath,
    /// The topic workspace (`workspace open --topic`), from anywhere in the repository.
    #[serde(default)]
    pub topic: Option<String>,
    /// A new or changed test that the test gate lets through without a kill or an assertion (with `--reason`).
    #[serde(default)]
    pub accept_test: Option<String>,
    /// Why the accepted test has value; it is recorded with the test and as an event.
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TestsRunIn {
    /// Only the tests whose name contains this (libtest's filter).
    #[serde(default)]
    pub filter: Option<String>,
    /// A folder in the repository.
    #[serde(default = "here")]
    pub repo: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TestShowIn {
    /// The test function's name (or libtest's, with its module path).
    pub name: String,
    /// A folder in the repository.
    #[serde(default = "here")]
    pub repo: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TestReviewIn {
    /// Review the index (what `git commit` takes), not the working tree.
    #[serde(default)]
    pub staged: bool,
    /// With `--staged`: the index file of a running `git commit` (`commit -a` and `commit <path>` use a file of their
    /// own). The client fills it from its `GIT_INDEX_FILE`, which git sets for the pre-commit hook.
    #[serde(default)]
    #[schemars(extend("x-env" = "GIT_INDEX_FILE"))]
    pub index: Option<ClientPath>,
    /// A folder in the repository or workspace.
    #[serde(default = "here")]
    pub repo: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MutantRecordIn {
    /// The file you changed, relative to the repository.
    pub file: String,
    /// The line of the change.
    pub line: u32,
    /// What the mutation does ("skip the root= check").
    pub description: String,
    /// The tests that failed with the mutation; none when it survived.
    #[serde(default)]
    pub killed_by: Vec<String>,
    /// A folder in the repository.
    #[serde(default = "here")]
    pub repo: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CloseIn {
    /// Discard uncommitted changes.
    #[serde(default)]
    pub force: bool,
    /// A folder in a workspace or in the repository.
    #[serde(default = "here")]
    pub repo: ClientPath,
    /// The topic workspace (`workspace open --topic`), from anywhere in the repository.
    #[serde(default)]
    pub topic: Option<String>,
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
    /// Mutter leaves the NVIDIA GPU alone (with intel or auto), so it can go to a VM (`gpu give`); from the next
    /// login.
    #[serde(default)]
    pub release_nvidia: bool,
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

/// What the layers' conditions are tested against now.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Conditions {
    /// ac or battery.
    pub power: String,
    /// The firmware mode chosen with Fn+Q that the policy follows; null when none.
    pub fnq: Option<String>,
    /// The workloads that run (game, build, vm).
    pub workload: Vec<String>,
    /// The package of the running kernel.
    pub kernel: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Rules {
    pub conditions: Conditions,
    /// Every layer in merge order (lowest first).
    pub layers: Vec<RuleState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuleState {
    pub layer: RuleLayer,
    /// `<folder>/<name>`, as `rules enable` and `rules disable` take it.
    pub id: String,
    /// Used now: on, and its conditions hold.
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuleDetail {
    pub layer: RuleState,
    /// The file as it is.
    pub text: String,
}

/// The desired state now: each knob a layer sets, with the chain of layers that set it, and what the daemon would write.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanView {
    pub conditions: Conditions,
    pub knobs: Vec<Knob>,
    /// The writes the daemon would make now (it keeps an Fn+Q choice).
    pub changes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuleIn {
    /// The layer: its id (`shipped/20-ac`), its name (`20-ac`) or its name without the order (`tuned`).
    pub layer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuleEditIn {
    /// Your layer `<two digits>-<name>` (a later number wins); the name of a shipped layer makes your layer of the
    /// same name, with its conditions, after it.
    pub layer: String,
    /// `knob=value` (knob ids as `legionctl knobs list`); `knob=` removes the knob from the layer.
    #[serde(default)]
    pub set: Vec<String>,
    /// The conditions, `power=ac,workload=game` (power, fnq, workload, kernel); `always` for none. Default: as they
    /// are.
    #[serde(default)]
    pub when: Option<String>,
    /// A system layer (`/etc/legionctl/configsets/`, polkit asks) instead of yours.
    #[serde(default)]
    pub system: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuleResetIn {
    /// Only this knob (an id or its path); default: every knob.
    #[serde(default)]
    pub knob: Option<String>,
    /// The system layers and the layers' on/off states (polkit asks) instead of your layers and holds.
    #[serde(default)]
    pub system: bool,
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
    /// The NVIDIA GPU's runtime power management; none without an NVIDIA GPU.
    pub nvidia_power: Option<NvidiaPower>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OnOff {
    On,
    Off,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GpuPowerIn {
    /// off: the NVIDIA GPU sleeps (D3cold) when no program uses it (refused while one does); on: it stays powered.
    pub state: OnOff,
    /// Print what would change; change nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FirmwareDevices {
    pub devices: Vec<FirmwareDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FirmwareUpdateIn {
    /// fwupd's device id or the device's name (`legionctl firmware status`).
    pub device: String,
    /// Print what would change; change nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RgbState {
    /// Where `OpenRGB`'s SDK server answers (`LEGIONCTL_OPENRGB`).
    pub address: String,
    /// The per-key RGB keyboard (ITE 8258, USB 048d:c988) is on the USB bus.
    pub keyboard_usb: bool,
    /// The SDK protocol both sides speak; none when `OpenRGB` does not answer.
    pub protocol: Option<u32>,
    /// The devices `OpenRGB` drives.
    pub controllers: Vec<String>,
    /// The profiles `OpenRGB` has saved; the knob rgb.profile names one.
    pub profiles: Vec<String>,
    /// Why `OpenRGB` cannot drive the keyboard now; none when it can.
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OverlayIn {
    /// on: the vitals overlay shows on the desktop and updates every 2 s; off: it goes.
    pub state: OnOff,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VfioDevices {
    pub items: Vec<VfioDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VfioBindIn {
    /// PCI addresses of one whole IOMMU group (`vfio status`).
    pub devices: Vec<String>,
    /// The VM runner that may lock the VM's memory; none when you bind by hand.
    #[serde(default)]
    pub pid: Option<u32>,
    /// The memory it may lock, in MiB (the VM's memory and some more).
    #[serde(default)]
    pub memlock_mib: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum BatteryMode {
    Standard,
    /// It stops near 80 %: a longer battery life on AC.
    Conservation,
    /// Faster charging (not with conservation).
    Rapid,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BatteryState {
    pub battery: Battery,
    /// Daily samples (oldest first): seconds since the epoch and health in percent.
    pub health_trend: Vec<(i64, f64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BatteryModeIn {
    pub mode: BatteryMode,
    /// Show what would happen; change nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum GpuName {
    /// The Intel iGPU (the Linux desktop and the laptop panel).
    Igpu,
    /// The NVIDIA RTX GPU (the external ports).
    Nvidia,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum GpuOwner {
    Linux,
    Windows,
    /// On vfio-pci, used by no one.
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GpuGiveIn {
    pub gpu: GpuName,
    pub to: GpuOwner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Autostart {
    /// Only `windows vm start` starts it.
    Off,
    /// It starts when you log in.
    Login,
    /// It starts when the machine starts, before you log in (your user's services run without a login).
    Boot,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AutostartIn {
    pub mode: Autostart,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WindowsVmBeginIn {
    /// The runner, which may lock the VM's memory.
    pub pid: u32,
    pub memlock_mib: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WindowsVmBegun {
    pub lines: Vec<String>,
    /// The PCI devices bound to vfio-pci for the VM (the Windows disk).
    pub devices: Vec<String>,
    /// The firmware's MSDM table (the Windows license) for the VM's ACPI tables.
    pub msdm: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VfioIn {
    /// PCI addresses to give back to their own drivers.
    pub devices: Vec<String>,
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
    /// Stop event: the reply goes on because a stop hook stopped it before (`stop_hook_active`); only a thread
    /// hard-block stops it again.
    #[serde(default)]
    pub again: bool,
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
pub struct DossierMigrateIn {
    /// List what the migration would do (moves, backups, clashes, the counts it compares) and change nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DossierRenameIn {
    /// The dossier's name now.
    pub name: String,
    /// The new name: a-z, 0-9 and '-', not in use. The dossier's id, bindings, workspaces and sessions stay.
    pub to: String,
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
    /// Where the session runs: host (a bwrap sandbox) or container (the repository's agent image from `legionctl
    /// workspace pack`, with the workspace and the dossier mounted).
    #[serde(default = "on_host", rename = "in")]
    pub inside: Inside,
    /// The NVIDIA GPU: its device nodes on the host and in a container (the host's driver); the GPU itself in a VM
    /// (VFIO: the desktop must not use it, see `vfio status`; the VM disk needs `workspace pack --as vm --gpu`).
    #[serde(default)]
    pub gpu: bool,
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
    /// Where the session runs: host (a bwrap sandbox) or container (the repository's agent image from `legionctl
    /// workspace pack`, with the workspace and the dossier mounted).
    #[serde(default = "on_host", rename = "in")]
    pub inside: Inside,
    /// The NVIDIA GPU: its device nodes on the host and in a container (the host's driver); the GPU itself in a VM
    /// (VFIO: the desktop must not use it, see `vfio status`; the VM disk needs `workspace pack --as vm --gpu`).
    #[serde(default)]
    pub gpu: bool,
    /// A tag for the run's live output events (`session.output`), so a client can follow them; `legionctl session
    /// run` sets one.
    #[serde(default)]
    pub tag: Option<String>,
    /// The ACP session of an earlier run (its `acp`) to go on in: the agent loads it, so it keeps the conversation.
    /// The run fails when the agent cannot load sessions.
    #[serde(default)]
    pub acp: Option<String>,
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
    /// The agent's ACP session: give it as `--acp` to the next run to go on in it.
    pub acp: Option<String>,
}

/// A command that the agent of a session offers (ACP `available_commands_update`); the client runs it with a
/// prompt that starts with `/<name>`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentCommand {
    pub name: String,
    pub description: String,
    /// What to type after the name, when the command takes input.
    pub hint: Option<String>,
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

/// What a process in an agent session did (the eBPF sensor in legionctl-system): started a program (`argv`), or
/// opened a file for the first time in the session (`path`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionExec {
    /// Seconds since the epoch.
    pub at: i64,
    pub pid: u32,
    /// The arguments of the program it started, cut at 512 bytes; empty for an open.
    pub argv: Vec<String>,
    /// The file it opened, as the program gave it (it can be relative to its folder), cut at 255 bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
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
    #[schemars(extend("x-argv" = true))]
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
pub struct SensorsIn {
    /// The CLI shows the view again each second until Ctrl+C; one call returns one view.
    #[serde(default)]
    pub watch: bool,
}

pub type Readings = Vec<Reading>;

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
pub struct KernelBenchIn {
    /// The suite: cpu, gpu or mixed (as `bench run`).
    pub scenario: Scenario,
    /// Duration of each run of the series (1-600).
    #[serde(default = "thirty")]
    pub seconds: u32,
    /// Start a series: each installed kernel once (`kernel try`) with the suite after login, then the default
    /// kernel again. Restarts the machine once per kernel, and once more at the end.
    #[serde(default)]
    pub all: bool,
    /// Stop the series of this suite; the default kernel starts at the next start.
    #[serde(default)]
    pub cancel: bool,
    /// Show the series; change nothing.
    #[serde(default)]
    pub dry_run: bool,
}

/// The newest run of one suite on each kernel, side by side; and what a series did or will do.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KernelBench {
    /// One run per kernel, the newest, in the order of the kernels' names.
    pub runs: Vec<BenchRun>,
    pub lines: Vec<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KernelBuildIn {
    /// The installed `linux-cachyos` kernel to build from (`linux-cachyos-bore-lto`): its PKGBUILD at its installed
    /// version.
    pub base: String,
    /// Kernel options to merge into its configuration (`CONFIG_<option>=<value>` lines, `merge_config.sh`).
    pub config_fragment: Option<ClientPath>,
    /// The new package is `linux-cachyos-<name>` (a-z, 0-9 and '-'); not an installed kernel.
    #[serde(default = "legionctl_name")]
    pub name: String,
    /// Show the plan; change nothing.
    #[serde(default)]
    pub dry_run: bool,
}

fn legionctl_name() -> String {
    "legionctl".into()
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
    /// Put the recommendation into a layer for the running kernel only (`tuned-<package>`, `kernel` condition).
    #[serde(default)]
    pub per_kernel: bool,
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
pub struct PackIn {
    /// The repository.
    #[serde(default = "here")]
    pub repo: ClientPath,
    /// The service whose image the agent layer goes on, when the repository builds more than one.
    #[serde(default)]
    pub service: Option<String>,
    /// What to make: the agent image (container) or the VM disk (vm).
    #[serde(default = "as_container", rename = "as")]
    pub kind: PackKind,
    /// With `--as vm`: also the NVIDIA driver in the disk, for sessions `--in vm --gpu` (a longer first boot).
    #[serde(default)]
    pub gpu: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PackKind {
    /// `localhost/<repo>/legionctl-agent:latest`, for sessions `--in container`.
    Container,
    /// `~/.local/share/legionctl/vm/<repo>.qcow2`, for sessions `--in vm`.
    Vm,
}

fn as_container() -> PackKind {
    PackKind::Container
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentsCheckIn {
    /// A team file (`teams/<team>.jsonc` of the repository's .legionctl, the user's config or the plugin); none:
    /// each agent config alone.
    #[serde(default)]
    pub team: Option<String>,
    /// The harness of the main agent: claude (members are teammates) or codex (members are Codex subagents).
    #[serde(default = "claude")]
    pub harness: Harness,
    /// The project whose repository's .legionctl config layer and teams apply.
    #[serde(default = "here")]
    pub project: ClientPath,
}

fn claude() -> Harness {
    Harness::Claude
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
pub struct KernelIn {
    /// The kernel's package (`linux-cachyos`), as `kernel list` shows it.
    pub name: String,
    /// Show what would happen; change nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KernelLineIn {
    /// The kernel's package (`linux-cachyos`), as `kernel list` shows it.
    pub name: String,
    /// The whole command line, in quotes (`"quiet rw root=UUID=..."`).
    pub line: String,
    /// Show what would happen; change nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FanCurveIn {
    /// Ten levels 1-10 (not RPM or percent), one for each point of the fan table from the lowest temperature to the
    /// highest; each equal to or higher than the one before; at least 3 at point 9 and 5 at point 10.
    pub levels: Vec<u8>,
    /// Check the levels and show the firmware call; change nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Switch {
    On,
    Off,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SwitchIn {
    pub state: Switch,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KnobHoldIn {
    /// Knob id (`cpu.epp`, `firmware.ppt_pl1_spl`, ...) or its path.
    pub knob: String,
    /// The value to hold it at; within the knob's choices or range (`knobs show`).
    pub value: String,
    /// Profile (JSONC); default: the installed one.
    pub profile: Option<ClientPath>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KnobIn {
    /// Knob id (`cpu.epp`, `firmware.ppt_pl1_spl`, ...) or its path.
    pub knob: String,
    /// Profile (JSONC); default: the installed one.
    pub profile: Option<ClientPath>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DisplayFixIn {
    /// The finding's id (`refresh-DP-3`, `antialiasing`, `xwayland-code`, ...), or `all`.
    pub finding: String,
    /// A monitor's new mode stays after the next login (GNOME's monitors.xml); without it, until then.
    #[serde(default)]
    pub persistent: bool,
    /// Show what would happen; change nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UndoIn {
    /// The event of the change (`events`): a machine change with writes (`machine.apply-power`, ...) or a change of
    /// your own settings (`display.change`, from `display fix`).
    pub event: i64,
}

/// One thing of the machine that a person attaches, starts, pauses or switches to (note 22).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Resource {
    /// vm, gpu, disk, configset or session.
    pub group: String,
    /// `vm:windows`, `mount:/mnt/windows-data`, `knob:sysctl.vm.swappiness`, ...: what `resources act` takes.
    pub id: String,
    pub name: String,
    pub state: String,
    /// What uses it: processes (`<pid> <program>`), sessions, an app.
    pub users: Vec<String>,
    /// The actions that apply in this state.
    pub actions: Vec<ResourceAction>,
}

/// An action of a resource: the operation it runs, with this input.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResourceAction {
    pub name: String,
    /// The operation (`knobs.release`), which runs as a call of its own.
    pub op: String,
    pub input: serde_json::Value,
    /// The operation's permission class.
    pub class: crate::Class,
    /// The operation has a dry run of its own.
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResourceList {
    pub resources: Vec<Resource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResourceActIn {
    /// The resource's id (`resources list`).
    pub id: String,
    /// One of its actions.
    pub action: String,
    /// Show what would happen; change nothing. An operation without a dry run of its own does not run: the answer
    /// shows its route.
    #[serde(default)]
    pub dry_run: bool,
}

/// The route of an action, and the routed operation's output once the operator ran it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Acted {
    pub op: String,
    pub input: serde_json::Value,
    /// The operator runs the routed operation; false for the dry run of an operation without its own.
    pub run: bool,
    /// The routed operation's output (null before it ran, or when it did not run).
    pub output: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DiskAction {
    /// The Windows disk's partitions to Linux: their automounts start.
    Attach,
    /// Away from Linux: their automounts and mounts stop.
    Detach,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WindowsDiskIn {
    pub action: DiskAction,
    /// Show what would happen; change nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetSaveIn {
    /// The set's name: a-z, 0-9 and '-'.
    pub name: String,
    /// Each kernel's image gets a boot entry for the set (it needs the boot takeover).
    #[serde(default)]
    pub boot: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetSwitchIn {
    /// The set's name; `previous` is the selection before the last switch (the undo).
    pub name: String,
    /// Show what would happen; change nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetIn {
    /// The set's name.
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphRefreshIn {
    /// The ref to make the graph of; HEAD when not given.
    #[serde(default = "head", rename = "ref")]
    pub rev: String,
    /// Return at once; the graph is made in the background (a job). The git hook uses it.
    #[serde(default)]
    pub background: bool,
    /// A folder in the repository (its main checkout or a worktree).
    #[serde(default = "here")]
    pub repo: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphQueryIn {
    /// Symbol search text.
    pub text: String,
    /// The ref to search; HEAD when not given.
    #[serde(default = "head", rename = "ref")]
    pub rev: String,
    /// Make the graph of the ref when it has none.
    #[serde(default)]
    pub build: bool,
    /// A folder in the repository.
    #[serde(default = "here")]
    pub repo: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphImpactIn {
    /// The symbol's stable key (`code graph query` shows it), such as `rust:crates/legionctl-core:graph::Node`.
    pub key: String,
    /// The ref; HEAD when not given.
    #[serde(default = "head", rename = "ref")]
    pub rev: String,
    /// Reverse dependency depth, 0 to 6.
    #[serde(default = "three")]
    pub depth: u8,
    /// Make the graph of the ref when it has none.
    #[serde(default)]
    pub build: bool,
    /// A folder in the repository.
    #[serde(default = "here")]
    pub repo: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphChangesIn {
    /// Two or more refs: the changes from each to the next. One ref: from its first parent. None: HEAD's last
    /// commit. For a rebase or a conflict, give the merge-base first.
    #[serde(default)]
    pub refs: Vec<String>,
    /// Make the graphs of the refs that have none.
    #[serde(default)]
    pub build: bool,
    /// Only the entities of this kind (`function`, `method`, `struct`, `config_key`, `file`, ...).
    #[serde(default)]
    pub kind: Option<String>,
    /// List every entity in the text; else the entities that are not code are a count per file.
    #[serde(default)]
    pub detail: bool,
    /// A folder in the repository.
    #[serde(default = "here")]
    pub repo: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AnnotateIn {
    /// The symbol's stable key.
    pub key: String,
    /// The annotation's value (JSON).
    pub value: String,
    /// note, review, test, loc, evidence, block, or a kind of your own.
    #[serde(default = "note")]
    pub kind: String,
    /// The ref where the symbol is as you annotate it; HEAD when not given.
    #[serde(default = "head", rename = "ref")]
    pub rev: String,
    /// A folder in the repository.
    #[serde(default = "here")]
    pub repo: ClientPath,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AnnotationsIn {
    /// The refs to check each annotation at; HEAD when none.
    #[serde(default)]
    pub refs: Vec<String>,
    /// Only the annotations of this stable key.
    #[serde(default)]
    pub key: Option<String>,
    /// A folder in the repository.
    #[serde(default = "here")]
    pub repo: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphPruneIn {
    /// Show what would be removed; remove nothing.
    #[serde(default)]
    pub dry_run: bool,
    /// A folder in the repository.
    #[serde(default = "here")]
    pub repo: ClientPath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum HookAction {
    /// Add the refresh line to the repository's git hooks.
    Install,
    /// Remove the refresh line; the other lines of each hook stay.
    Remove,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphHookIn {
    pub action: HookAction,
    /// A folder in the repository.
    #[serde(default = "here")]
    pub repo: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanCheckIn {
    /// The ref whose plan and code graph to check; HEAD when not given.
    #[serde(default = "head", rename = "ref")]
    pub rev: String,
    /// Make the graph of the ref when it has none.
    #[serde(default)]
    pub build: bool,
    /// A folder in the repository.
    #[serde(default = "here")]
    pub repo: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanStatusIn {
    /// One feature (its id); all when not given.
    #[serde(default)]
    pub feature: Option<String>,
    /// The ref to measure; HEAD when not given.
    #[serde(default = "head", rename = "ref")]
    pub rev: String,
    /// Also list the changes from this ref to `ref` that touch each work item.
    #[serde(default)]
    pub since: Option<String>,
    /// Make the graphs of the refs that have none.
    #[serde(default)]
    pub build: bool,
    /// A folder in the repository.
    #[serde(default = "here")]
    pub repo: ClientPath,
}

fn head() -> String {
    "HEAD".into()
}

fn three() -> u8 {
    3
}

fn note() -> String {
    "note".into()
}

fn main0() -> String {
    "main0".into()
}

fn edit_tools() -> String {
    "Edit|Write|MultiEdit|NotebookEdit".into()
}

fn four() -> u32 {
    4
}

fn on_trigger() -> crate::Pace {
    crate::Pace::OnTrigger
}

fn reminder() -> crate::MessageKind {
    crate::MessageKind::Reminder
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThreadStartIn {
    /// The task, in a few words.
    pub title: String,
    /// The dossier of the task; default: the caller's dossier.
    #[serde(default)]
    pub dossier: Option<String>,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThreadIdIn {
    /// The thread's id (`thread list`).
    pub thread: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThreadJoinIn {
    pub thread: i64,
    /// The member's name in the thread (`observer0`).
    pub name: String,
    pub archetype: crate::Archetype,
    /// The agent session of the member, which its hooks and calls carry.
    #[serde(default)]
    pub member_session: Option<String>,
    #[serde(default = "on_trigger")]
    pub pace: crate::Pace,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThreadMemberIn {
    pub thread: i64,
    /// The member's name in the thread.
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThreadSendIn {
    /// A member name of the thread, or a peer's session id (`thread peers`).
    pub to: String,
    pub kind: crate::MessageKind,
    /// The message: the fact you verified and how the receiver clears it.
    pub text: String,
    /// The thread; default: the caller's only open thread.
    #[serde(default)]
    pub thread: Option<i64>,
    /// For a hard-block: the tool it refuses until the receiver acks it (`workspace_checkpoint`).
    #[serde(default)]
    pub tool: Option<String>,
    /// For a notice: the watch rule whose step you did for the receiver; the message then tells the counts.
    #[serde(default)]
    pub rule: Option<i64>,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThreadAckIn {
    /// The message id.
    pub id: i64,
    /// A note back to the sender.
    #[serde(default)]
    pub reply: Option<String>,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThreadDeclareIn {
    pub word: crate::Declared,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThreadPaceIn {
    pub thread: i64,
    /// The member's name in the thread.
    pub name: String,
    pub pace: crate::Pace,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuleAddIn {
    pub thread: i64,
    /// The member that watches and sends the message.
    pub member: String,
    /// The Bash command that must come between the counted tool calls (`make lint`).
    pub expect: String,
    /// The member it watches.
    #[serde(default = "main0")]
    pub watched: String,
    /// The tool calls that count, `|` between names.
    #[serde(default = "edit_tools")]
    pub tools: String,
    /// A part of the path that a counted call touches (`crates/`); empty: every path.
    #[serde(default)]
    pub path: String,
    /// The count of calls without the command at which the rule sends its message.
    #[serde(default = "four")]
    pub every: u32,
    #[serde(default = "reminder")]
    pub kind: crate::MessageKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuleRmIn {
    /// The rule's id (`thread rules list`).
    pub id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PeersIn {
    /// Peers on this dossier, in place of the caller's own scope.
    #[serde(default)]
    pub dossier: Option<String>,
    /// Peers in this topic workspace of the caller's dossier.
    #[serde(default)]
    pub topic: Option<String>,
    /// Peers that are members of this thread.
    #[serde(default)]
    pub thread: Option<i64>,
    /// Where the caller works.
    #[serde(default = "here")]
    pub repo: ClientPath,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ObserveIn {
    /// A peer's session id, or a member name in one of the caller's threads.
    pub peer: String,
    /// Where the caller works.
    #[serde(default = "here")]
    pub repo: ClientPath,
    #[serde(default)]
    pub session: SessionId,
}

/// One tool call of an agent, from Claude Code's `PostToolUse` hook.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentToolIn {
    #[serde(default)]
    pub session: SessionId,
    /// The agent's working directory.
    #[serde(default = "here")]
    pub project: ClientPath,
    pub tool: String,
    /// The file it wrote.
    #[serde(default)]
    pub path: Option<String>,
    /// The Bash command.
    #[serde(default)]
    pub command: Option<String>,
    /// `SendMessage`: the receiver.
    #[serde(default)]
    pub to: Option<String>,
    /// `SendMessage`: the text.
    #[serde(default)]
    pub message: Option<String>,
    /// False when the tool reports that it failed.
    #[serde(default)]
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThreadGateIn {
    #[serde(default)]
    pub session: SessionId,
    /// The tool that the agent is about to call.
    pub tool: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThreadImportIn {
    /// The thread's id (`thread list`).
    pub thread: i64,
    /// The account profile of the copy (`agents accounts list`).
    pub account: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ViewTextIn {
    /// The view file's JSONC text (stdin when not given).
    pub content: StdinText,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ViewSaveIn {
    /// The view file's JSONC text (stdin when not given).
    pub content: StdinText,
    /// Save into this repository's layer (`.legionctl/views/`), shared with the code; default: the user's layer.
    #[serde(default)]
    pub repo: Option<ClientPath>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ViewsListIn {
    /// A folder in a repository: its layer comes first.
    #[serde(default = "here")]
    pub repo: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ViewRenderIn {
    /// The view's name (`views list`).
    pub name: String,
    /// The view's parameters as `name=value`.
    #[serde(default)]
    pub params: Vec<String>,
    /// A folder in a repository: its layer comes first.
    #[serde(default = "here")]
    pub repo: ClientPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ViewPlaceIn {
    /// The thread's id (`thread list`).
    pub thread: i64,
    /// The view's name (`views list`).
    pub name: String,
    /// The view's parameters as `name=value`.
    #[serde(default)]
    pub params: Vec<String>,
    /// A folder in a repository: its layer comes first.
    #[serde(default = "here")]
    pub repo: ClientPath,
}

/// A view file of a layer.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ViewInfo {
    pub name: String,
    /// repository or user.
    pub layer: String,
    pub file: std::path::PathBuf,
    /// The widget; none when the file is not a view.
    pub widget: Option<ViewWidget>,
    /// Why the file is not a view.
    pub error: Option<String>,
}
