//! Inputs and outputs of the operations. A field's doc comment is its help text on every surface.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::wire::FactScope;
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
pub struct ExplainIn {
    /// Dotted key, such as hooks.bindings.prompt or load.busy.
    pub key: String,
}

/// Which settings `config settings` lists: all, or those that match every filter given.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SettingsIn {
    /// Words in a setting's name, key or help (any case).
    #[serde(default)]
    pub search: Option<String>,
    /// Only one module: the key's first part (`dossier`, `gateway`).
    #[serde(default)]
    pub module: Option<String>,
    /// Only this setting, by its key.
    #[serde(default)]
    pub key: Option<String>,
    /// Only the settings that a view or an operation group uses (the gear of a view).
    #[serde(default)]
    pub view: Option<String>,
    /// Only the settings whose value is not the built-in default.
    #[serde(default)]
    pub changed: bool,
}

/// Every setting with its value now and where that value comes from (DESIGN-20).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SettingsView {
    /// The areas that the project manages itself (`project.owns`): legionctl leaves them alone.
    #[serde(default)]
    pub owned: Vec<legionctl_core::config::Area>,
    pub items: Vec<SettingRow>,
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SettingRow {
    #[serde(flatten)]
    pub setting: legionctl_core::settings::Setting,
    /// The value in use.
    pub value: serde_json::Value,
    /// The value in use in a line: plain text as it is, other values as JSON, a long list or map as its count.
    pub text: String,
    /// The value in use for a form: the value, and empty text for none.
    pub edit: serde_json::Value,
    /// Where the value in use comes from: defaults, computer, user or project.
    pub from: String,
    /// Only this computer's administrator may change it.
    pub locked: bool,
    /// Why the value in use does not work, with the step that fixes it (its deep check); empty when it works.
    pub finding: String,
    /// A list's entries, each with where it comes from (a list of rules: pattern, reason, from); a map's: name, key,
    /// text, edit, from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<serde_json::Value>,
    /// What the row offers besides Change and Reset (a map: its entries in a view of their own).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ResourceAction>,
}

/// The layer that a change goes into.
fn for_you() -> legionctl_core::settings::Scope {
    legionctl_core::settings::Scope::User
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConfigSetIn {
    /// Dotted key of the setting, or of an entry in a map setting (`reminders.ste100.enabled`).
    pub key: String,
    /// The new value: text for a setting that takes text, else JSON (`5`, `true`, `["a", "b"]`).
    #[schemars(extend("x-setting-value" = "key"))]
    pub value: serde_json::Value,
    /// For whom: user (you), project (the repository's settings, for everybody who works in it) or computer (every
    /// user of this computer; the system asks for an administrator's password).
    #[serde(default = "for_you")]
    pub scope: legionctl_core::settings::Scope,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConfigUnsetIn {
    /// Dotted key of the setting.
    pub key: String,
    /// The layer to remove it from: user, project or computer.
    #[serde(default = "for_you")]
    pub scope: legionctl_core::settings::Scope,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConfigItemIn {
    /// Dotted key of a list setting (`actions.forbid`, `session.hidden`), or of a map setting (remove: the entry's name).
    pub key: String,
    /// The entry: for a list of rules its pattern (`git:push`, `op:boot.*`), else the item (text or JSON).
    pub value: serde_json::Value,
    /// For a list of rules: why, in a few words.
    #[serde(default)]
    pub reason: Option<String>,
    /// For a git: or cmd: rule: only for the sessions that hold this capability set (rust-dev).
    #[serde(default)]
    pub set: Option<String>,
    /// For whom: user, project or computer.
    #[serde(default = "for_you")]
    pub scope: legionctl_core::settings::Scope,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConfigRevertIn {
    /// The change to undo: its number in `config history`.
    pub id: i64,
}

fn twenty() -> u32 {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConfigHistoryIn {
    /// Only this setting (and the keys inside it).
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default = "twenty")]
    pub limit: u32,
}

/// A setting changed: the change kept in the history, and what the person needs to know.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SettingChanged {
    pub change: legionctl_core::settings::Change,
    pub lines: Vec<String>,
}

/// The JSON Schema of a settings layer file.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct LayerSchema(pub serde_json::Value);

/// The changes of settings, newest first.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SettingsHistory {
    pub items: Vec<legionctl_core::settings::Change>,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceOpenIn {
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
    /// The topic workspace (`workspace open --topic`), from anywhere in the repository.
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MergeIn {
    /// The topic workspace (`workspace open --topic`), from anywhere in the repository.
    #[serde(default)]
    pub topic: Option<String>,
    /// The user's merge (no agent session): the dossier of the workspace; not needed when one dossier has a
    /// workspace there.
    #[serde(default)]
    pub dossier: Option<String>,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CheckpointIn {
    /// Commit message: what this change set does (from the plan entry).
    pub message: String,
    /// The topic workspace (`workspace open --topic`), from anywhere in the repository.
    #[serde(default)]
    pub topic: Option<String>,
    /// A new or changed test that the test gate lets through without a kill or an assertion (with `--reason`); repeat
    /// it for more tests.
    #[serde(default)]
    #[schemars(extend("x-option" = true))]
    pub accept_test: Vec<String>,
    /// Why the accepted test has value; it is recorded with the test and as an event. One for all accepted tests, or
    /// one per `--accept-test`, in the same order.
    #[serde(default)]
    #[schemars(extend("x-option" = true))]
    pub reason: Vec<String>,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TestsRunIn {
    /// Only the tests whose name contains this (libtest's filter).
    #[serde(default)]
    pub filter: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ChecksRunIn {
    /// The checks by name (`code checks list`); none: each check but deploy and install.
    #[serde(default)]
    pub checks: Vec<String>,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CheckShowIn {
    /// The run's id (`<job>-<check>`).
    pub run: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TestShowIn {
    /// The test function's name (or libtest's, with its module path).
    pub name: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CloseIn {
    /// Discard uncommitted changes.
    #[serde(default)]
    pub force: bool,
    /// Show the workspace that close would remove and what it would discard; change nothing.
    #[serde(default)]
    pub dry_run: bool,
    /// The topic workspace (`workspace open --topic`), from anywhere in the repository.
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ForgetIn {
    /// The workspace's folder, as doctor and `workspace list` name it.
    pub path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SetupApplyIn {
    /// Install ctx with its signed installer if it is missing.
    pub install_ctx: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ServiceIn {
    /// The service: legionctl-system, legionctl-operator or clauth daemon.
    pub service: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AccountIn {
    /// The clauth account: a-z, 0-9 and '-'.
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AccountLoginIn {
    /// The clauth account: a-z, 0-9 and '-'; an account clauth has already logs in again.
    pub name: String,
    /// Codex's login, not Claude Code's.
    #[serde(default)]
    pub codex: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AccountUsageIn {
    /// One account; none: each.
    #[serde(default)]
    pub account: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThreadAccountIn {
    /// The thread's id (`thread list`).
    pub thread: i64,
    /// An operator member: it runs on the account from its next wake. None: the chat bar's account.
    #[serde(default)]
    pub member: Option<String>,
    /// A clauth account (`agents accounts list`); none: the current login.
    #[serde(default)]
    pub account: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IdIn {
    /// The reminder's or hook action's id (`reminders list`, `hooks list`).
    pub id: String,
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
    pub findings: Vec<Finding>,
}

/// One finding of `doctor`: what is wrong, in words for the user, and the actions that fix it; with no action it is
/// a state that legionctl cannot change.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Finding {
    pub text: String,
    pub actions: Vec<ResourceAction>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Answer {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalAnswerIn {
    /// The question's number: `ask` of its `approval.ask` event (the app's Activity, `legionctl events`).
    pub ask: u64,
    pub answer: Answer,
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
    /// Set when fwupd is not installed: what to install (no devices then).
    pub missing: Option<String>,
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
    /// The VM runs on this overlay (a repair), not on the disk itself.
    pub overlay: Option<OverlayDisk>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OverlayDisk {
    /// The qcow2 file that takes Windows' writes.
    pub file: String,
    /// The disk's block device under it, only read.
    pub disk: String,
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
    /// Session-start event: how the session started (`startup`, `resume`, `clear`, `compact`).
    #[serde(default)]
    pub source: Option<String>,
    /// The agent session that started this one from its shell (Codex from a Claude Code session: the
    /// `CLAUDE_CODE_SESSION_ID` it inherited); a session with no dossier of its own reads that session's.
    #[serde(default)]
    pub lead: Option<SessionId>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct DossierReleaseIn {
    #[serde(default)]
    pub session: SessionId,
    /// Claude Code's `SessionEnd` reason (the session-end hook sets it): after `clear` and `resume` the agent goes on
    /// in another session, so a pool that the session leads as main0 stays open; any other reason, or none, ends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DossierCreateIn {
    /// Dossier name: a-z, 0-9 and '-', starting with a letter or digit; a name in use is refused.
    pub name: String,
    /// Link the dossier's home into the project as `agentWorkspaces`.
    #[serde(default)]
    pub link: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DossierUseIn {
    /// Dossier name: a-z, 0-9 and '-'. For a session, a name that does not exist makes a new dossier from the
    /// pattern; with no session (the app, a terminal), the dossier must exist (dossier create).
    pub name: String,
    /// Take it over from another live session; for an agent, the operator asks the user first.
    #[serde(default)]
    #[schemars(extend("x-approval" = true))]
    pub steal: bool,
    #[serde(default)]
    pub session: SessionId,
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
    /// A thread-observer's backfill into the main agent's dossier: the caller's member name. One line, marked
    /// `(by <member>)`, only while main0 is idle; else the call is refused and the member sends a nudge.
    #[serde(default)]
    pub member: Option<String>,
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

/// One row of `about`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AboutItem {
    /// What the row is about: operator, daemon, machine, licenses, source.
    pub part: String,
    pub value: String,
    /// ok, or warn when the part needs you (a daemon that does not match, or no daemon).
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FeedbackIn {
    /// What you see or want.
    pub note: String,
    /// The dossier of the entry.
    pub dossier: String,
    /// What the app showed, for the note's front matter (space, tab, view, version, app build); the operator adds the
    /// time and the builds of the operator and the daemon.
    #[serde(default)]
    pub about: std::collections::BTreeMap<String, String>,
    /// The app's state as JSON (the tab's view, its data, errors): state.json, with the operator's last events.
    #[serde(default)]
    pub state: Option<String>,
    /// Ask the desktop for a screenshot (you pick the window): screenshot.png.
    #[serde(default)]
    pub screenshot: bool,
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
pub struct DossierHealthIn {
    /// Dossier to audit; default: this session's.
    #[serde(default)]
    pub dossier: Option<String>,
    /// At most this many finding lines; the answer says how many it left out.
    #[serde(default = "forty")]
    pub limit: u32,
    #[serde(default)]
    pub session: SessionId,
}

fn forty() -> u32 {
    40
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CompactIn {
    /// What started the compaction: `manual` (/compact) or `auto`.
    #[serde(default)]
    pub trigger: Option<String>,
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
    /// A clauth account (`agents accounts list`): its login instead of the current one.
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
    /// A capability set to request besides the main agent's, its harness's and its languages' (repeat it); a set that
    /// the grant refuses stops the start with the reason.
    #[serde(default)]
    #[schemars(extend("x-option" = true))]
    pub capability: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunnablesShowIn {
    /// A session id (`session list`), or a thread member as `<thread>/<member>`.
    pub target: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ScopeIn {
    /// The operations and brokered tools (`<server>/<tool>`) to check; none: every public operation.
    #[serde(default)]
    pub tools: Vec<String>,
}

/// The caller's scope: its toolsets, and the tools it does not hold with the toolset that a run
/// asks the user for.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Scope {
    /// The toolsets (capability sets) of its grant; the read class is in every scope.
    pub sets: Vec<String>,
    /// Tool -> the toolset that holds it; empty: no toolset holds it, so a run of it is refused.
    pub missing: std::collections::BTreeMap<String, String>,
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
    /// A clauth account (`agents accounts list`): its login instead of the current one.
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
    /// A capability set to request besides the main agent's, its harness's and its languages' (repeat it); a set that
    /// the grant refuses stops the start with the reason.
    #[serde(default)]
    #[schemars(extend("x-option" = true))]
    pub capability: Vec<String>,
    /// A tag for the run's live output events (`session.output`), so a client can follow them; `legionctl session
    /// run` sets one.
    #[serde(default)]
    pub tag: Option<String>,
    /// The ACP session of an earlier run (its `acp`) to go on in: the agent loads it, so it keeps the conversation.
    /// The run fails when the agent cannot load sessions.
    #[serde(default)]
    pub acp: Option<String>,
    /// The agent's model for this run (ACP `session/set_model`: `haiku`); none: the account's default; the run
    /// fails when the agent does not take it.
    #[serde(default)]
    pub model: Option<String>,
    /// The user's thread of the run (the app's chat): the run works on the thread's dossier and in its folder, goes
    /// on in the thread's last ACP session, and the prompt and the answer (or the failure) are rows of its log.
    #[serde(default)]
    pub thread: Option<i64>,
    /// Bind the agent to these operations: its only tools are legionctl's MCP tools over them (no file, shell or web
    /// tools of its own, no settings, plugins or other MCP servers). None: the agent's own tools and the session's MCP
    /// servers.
    #[serde(default)]
    pub tools: Vec<String>,
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
    /// Continue on this clauth account instead (its rate limits, its login); the conversation goes on.
    pub account: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GateIn {
    /// The git arguments, as the command got them.
    #[schemars(extend("x-argv" = true))]
    pub args: Vec<String>,
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
    /// Only events of this kind (`dossier.use`); repeat it for more kinds. Every kind when not given.
    #[serde(default)]
    #[schemars(extend("x-option" = true))]
    pub kind: Vec<String>,
    /// Only events at this time or later: seconds since 1970 (as `at` shows them), or a local time such as
    /// `2026-10-21 04:30`.
    #[serde(default)]
    pub since: Option<String>,
    /// Only events before this time, in the same forms as `since`.
    #[serde(default)]
    pub until: Option<String>,
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

/// The start of a brokered MCP tool call (`legionctl mcp-proxy`): its row in the call record before it runs.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CallBeginIn {
    /// The caller's call id, to retry a call; none: a new id.
    #[serde(default)]
    pub call: Option<String>,
    /// The agent session (`LEGIONCTL_SESSION`); a sandbox's own, whatever it sends.
    #[serde(default)]
    pub session: SessionId,
    pub server: String,
    pub tool: String,
    /// BLAKE2b-128 of the arguments' JSON, hex.
    pub args_hash: String,
    /// The proxy refuses it (`actions.allow` and `actions.forbid` of its tools): the record keeps it as refused, and no toolset is
    /// asked for.
    #[serde(default)]
    pub refused: bool,
}

/// A started call: its id, and the recorded result when the id is of a call that ended (a retry gets it again).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CallStart {
    pub call: String,
    #[serde(default)]
    pub replay: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CallEndIn {
    pub call: String,
    pub outcome: Outcome,
    /// Size of the result's JSON.
    pub bytes: u64,
    /// The result, kept for a retry of the call.
    #[serde(default)]
    pub output: Option<serde_json::Value>,
}

/// One call of the call record, as `tools calls` lists it (no arguments, only their hash).
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

/// A tool call as the cache knows it (DESIGN/03): the operator builds the key from these, from the git state of the
/// call's folder for a `workspace` tool, and from the caller's session, so one sandbox cannot put entries under another's keys.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CachedCall {
    /// The server: its command line (for a package, it names the store version).
    pub server: String,
    pub tool: String,
    /// The call's arguments.
    pub args: serde_json::Value,
    /// The result depends on the git state of the call's folder (a tool that reads the working copy).
    #[serde(default)]
    pub workspace: bool,
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
    /// The call has no end yet: it runs, or the operator stopped during it.
    Running,
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
    /// A container's Docker state (running, paused, exited) or a VM's virsh state (running, paused, shut off).
    pub state: String,
    /// The app it belongs to, when legionctl recognizes it (`WinBoat` by its image).
    pub consumer: Option<String>,
    /// Host memory it holds while it runs: a container's use without its page cache, a VM's resident memory.
    pub memory_bytes: Option<u64>,
    /// Why `memory_bytes` is not known while it runs.
    pub memory_unknown: Option<String>,
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
    /// Show the changed lines; write nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunnablesCheckIn {
    /// A team file (`teams/<team>.jsonc` of the repository's .legionctl, your config or the plugin); none:
    /// each agent config alone.
    #[serde(default)]
    pub team: Option<String>,
    /// The harness of the main agent: claude (members are teammates) or codex (members are Codex subagents).
    #[serde(default = "claude")]
    pub harness: Harness,
}

fn claude() -> Harness {
    Harness::Claude
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct AgentUsageIn {
    /// Transcripts changed in the last days.
    #[serde(default = "seven")]
    pub days: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PluginInstallIn {
    /// `<plugin>@<marketplace>`, both allowed in `plugins.marketplaces`.
    pub id: String,
    /// Where Claude Code records the plugin: user, project or project-local.
    pub scope: SettingsScope,
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

/// A Claude Code session as clauth lists it (`clauth sessions`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClauthSession {
    pub id: String,
    /// The folder it worked in.
    pub workspace: String,
    /// When it last wrote.
    pub updated_at: i64,
    /// The clauth account it last ran on; none: clauth does not know.
    pub profile: Option<String>,
}

/// An account: the current login (`current`, named by the clauth profile that holds it) or another clauth profile
/// (`clauth`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Account {
    pub name: String,
    pub source: String,
    /// The current login, or clauth's active profile.
    pub active: bool,
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
    /// Only calls of this kind: `op` (legionctl's operations), `tool` (brokered MCP tools) or `model` (the gateway).
    #[serde(default)]
    pub kind: Option<String>,
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
    /// The call id (a retry sends it again).
    pub call_id: String,
    /// `op`, `tool` or `model`.
    pub kind: String,
    /// When it started (ms).
    pub at: i64,
    /// The folder it worked in; empty when the record does not say.
    #[serde(default)]
    pub folder: String,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
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
    /// Processes whose open files the reader may not see (another user's, or not dumpable): not in any `users`.
    pub not_visible: usize,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OverlayAction {
    /// Whether there is an overlay, its size and whether Windows ran on the disk since it was made.
    Status,
    /// Make one: the next VM starts write to it, the disk stays as it is.
    Create,
    /// Write it into the disk, then remove it.
    Commit,
    /// Remove it: the disk stays as it was.
    Drop,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WindowsOverlayIn {
    pub action: OverlayAction,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WindowsImageIn {
    /// A new file for the image (qcow2).
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RepairStep {
    /// `sfc /scannow`: Windows' system files checked and repaired.
    Sfc,
    /// `DISM /Online /Cleanup-Image /RestoreHealth`: the component store repaired (from Windows Update).
    Dism,
    /// `chkdsk C: /scan`: the file system checked while Windows runs.
    Chkdsk,
    /// `bcdboot` typed into Windows RE's command prompt: Windows' boot files on the EFI partition made again.
    Boot,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WindowsRepairIn {
    pub step: RepairStep,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WindowsRestoreIn {
    /// The restore point's number (`windows restore list`).
    pub point: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum HostAction {
    /// Shut down the whole machine.
    Shutdown,
    /// Restart the whole machine into Linux.
    Restart,
    /// Restart the whole machine into Windows on the machine itself (bare metal).
    RestartWindows,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WindowsHostIn {
    pub action: HostAction,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphTreeIn {
    /// A folder or a file of the repository ("": its top): the level under it.
    #[serde(default)]
    pub prefix: String,
    /// The ref; HEAD when not given.
    #[serde(default = "head", rename = "ref")]
    pub rev: String,
    /// Make the graph of the ref when it has none.
    #[serde(default)]
    pub build: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphChangesIn {
    /// Two or more refs: the changes from each to the next. One ref: from its first parent. None: HEAD's last
    /// commit. With --three-way: `<base> <ours> <theirs>`.
    #[serde(default)]
    pub refs: Vec<String>,
    /// For a rebase or a conflict: base to ours, base to theirs, and the entities that both sides changed.
    #[serde(default)]
    pub three_way: bool,
    /// Make the graphs of the refs that have none.
    #[serde(default)]
    pub build: bool,
    /// Only the entities of this kind (`function`, `method`, `struct`, `config_key`, `file`, ...).
    #[serde(default)]
    pub kind: Option<String>,
    /// List every entity in the text; else the entities that are not code are a count per file.
    #[serde(default)]
    pub detail: bool,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphPruneIn {
    /// Show what would be removed; remove nothing.
    #[serde(default)]
    pub dry_run: bool,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanCheckIn {
    /// The ref whose plan and code graph to check; HEAD when not given.
    #[serde(default = "head", rename = "ref")]
    pub rev: String,
    /// Make the graph of the ref when it has none.
    #[serde(default)]
    pub build: bool,
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
    /// A team file (`vanilla`): its members join the thread and its flow applies.
    #[serde(default)]
    pub team: Option<String>,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThreadRepoIn {
    /// The user's thread (`thread list`).
    pub thread: i64,
    /// The ref that the thread's branch starts from; default: the repository's current branch. Not for a plain
    /// folder.
    #[serde(default)]
    pub base: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThreadBranchIn {
    pub thread: i64,
    /// The last log row of the parent that the branch keeps (`thread show`).
    pub at: i64,
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThreadCompensateIn {
    /// The effect's id (`thread show`, effects).
    pub effect: i64,
    #[serde(default)]
    pub session: SessionId,
}

/// A team mode hook of the lead: `idle` (`TeammateIdle`) or `task` (`TaskCompleted`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TeammateIn {
    pub event: String,
    /// The teammate's name.
    #[serde(default)]
    pub teammate: Option<String>,
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
    /// A member name of the thread, or a peer's id (`thread peers`).
    pub to: String,
    pub kind: crate::MessageKind,
    /// The message: the fact you verified and how the receiver clears it.
    pub text: String,
    /// The thread; default: the caller's only open thread.
    #[serde(default)]
    pub thread: Option<i64>,
    /// For a hard-block: the operation (`workspace.checkpoint`) or tool (`Bash`) it refuses until the receiver acks it.
    #[serde(default)]
    pub tool: Option<String>,
    /// For a notice: the watch rule whose step you did for the receiver; the message then tells the counts.
    #[serde(default)]
    pub rule: Option<i64>,
    /// Team mode: a teammate shares its lead's session, so it names itself (its member name in the lead's thread).
    #[serde(default)]
    pub teammate: Option<String>,
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
    #[serde(default)]
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ObserveIn {
    /// A peer's id (`thread peers`: a session id, or `<session>/<agent id>` for a subagent), or a member name in one
    /// of the caller's threads.
    pub peer: String,
    #[serde(default)]
    pub session: SessionId,
}

/// One tool call of an agent, from Claude Code's `PostToolUse` hook.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentToolIn {
    #[serde(default)]
    pub session: SessionId,
    /// The subagent that called the tool (Claude Code's `agent_id`); none for the session's main agent.
    #[serde(default)]
    pub agent: Option<String>,
    /// The subagent's type (Claude Code's `agent_type`, for example `Explore`).
    #[serde(default)]
    pub agent_type: Option<String>,
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
    /// `Skill`: the skill the agent loaded (`legionctl:<name>`); its toolsets join a harness agent's scope.
    #[serde(default)]
    pub skill: Option<String>,
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
    /// The clauth account of the copy (`agents accounts list`).
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
    /// Save into the project's views (`.legionctl/views/`), shared with its code; default: your views.
    #[serde(default)]
    pub project: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ViewRenderIn {
    /// The view's name (`views list`).
    pub name: String,
    /// The view's parameters as `name=value`.
    #[serde(default)]
    pub params: Vec<String>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThreadAgentIn {
    /// The thread's id (`thread list`).
    pub thread: i64,
    /// A name of config `session.acp`.
    pub agent: String,
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
    /// Its words, the intent it answers and the navigator's space that lists it.
    pub title: Option<String>,
    pub intent: Option<String>,
    pub space: Option<String>,
    /// Why the file is not a view.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ViewVersionIn {
    /// The view's name (`views list`).
    pub name: String,
    /// undo, redo, reset (to the pinned version, or else the original), pin (reset goes back to the version that shows), or
    /// a version number.
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RlcdAskIn {
    /// The text the questions are about (read from stdin when not given). Secrets are removed before it is sent.
    pub state: StdinText,
    /// The questions: `<id>:choice:<a>[=<meaning>],<b>[:<instructions>]`, `<id>:yesno[:<instructions>]`,
    /// `<id>:score[:<instructions>]`.
    pub questions: Vec<String>,
    /// The backend (`rlcd.backends.<name>`); default: `rlcd.backend`.
    #[serde(default)]
    pub backend: Option<String>,
    /// For a manual check with a key that is not in the keyring: `--key-env <VAR>` names the client's environment
    /// variable that holds it; the key is never on the command line or in the audit. Default: the backend's keyring
    /// entry.
    #[serde(default)]
    #[schemars(extend("x-secret" = true, "x-from-env" = true))]
    pub key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RlcdDecisionsIn {
    /// The newest this many.
    #[serde(default = "fifty")]
    pub limit: u32,
}

fn fifty() -> u32 {
    50
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FactSetIn {
    /// What it is about: `app`, `view:<name>`, `intent:<words>`, a code graph key (`rust:.:add`).
    pub entity: String,
    /// The attribute of the entity.
    #[serde(default = "value_word")]
    pub attribute: String,
    /// The value as JSON.
    pub value: String,
    /// Where it holds: global (all your work), project (the repository of this folder), workspace (its worktree), dossier
    /// (--dossier) or thread (--thread).
    #[serde(default = "global")]
    pub scope: FactScope,
    /// The dossier scope.
    #[serde(default)]
    pub dossier: Option<String>,
    /// The thread scope.
    #[serde(default)]
    pub thread: Option<i64>,
    /// When it starts to be true (ms since the epoch); default now.
    #[serde(default)]
    pub valid_from: Option<i64>,
    /// When it stops to be true (ms since the epoch); default open.
    #[serde(default)]
    pub valid_to: Option<i64>,
    /// The commit (a ref of the repository) where it starts to hold; then it holds at the refs that contain it.
    #[serde(default)]
    pub from_ref: Option<String>,
    /// The commit where it stops to hold.
    #[serde(default)]
    pub to_ref: Option<String>,
    /// A code graph key: the fact holds while the symbol's content is the same as at --from-ref (default HEAD).
    #[serde(default)]
    pub anchor: Option<String>,
    /// Why: the request, the event or the commit.
    #[serde(default)]
    pub evidence: Option<String>,
    /// 1 for a user or rule fact; a probability for a model's.
    #[serde(default = "certain")]
    pub confidence: f64,
    /// Correct the fact with this id (bitemporal: the old row stays in the history with its recorded time).
    #[serde(default)]
    pub corrects: Option<i64>,
}

fn value_word() -> String {
    "value".into()
}

fn global() -> FactScope {
    FactScope::Global
}

fn certain() -> f64 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FactsAtIn {
    /// An entity, or a prefix with `*` at its end (`view:*`); default all.
    #[serde(default)]
    pub entity: Option<String>,
    /// The dossier scope.
    #[serde(default)]
    pub dossier: Option<String>,
    /// The thread scope.
    #[serde(default)]
    pub thread: Option<i64>,
    /// The valid time to look at (ms since the epoch); default now.
    #[serde(default)]
    pub time: Option<i64>,
    /// What the store knew at this time (ms since the epoch); default now.
    #[serde(default)]
    pub recorded: Option<i64>,
    /// The ref of ref-bound and anchored facts; default HEAD.
    #[serde(default, rename = "ref")]
    pub rev: Option<String>,
    /// Only the winner of each entity and attribute.
    #[serde(default)]
    pub winners: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FactsHistoryIn {
    /// An entity, or a prefix with `*` at its end.
    pub entity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FactsDiffIn {
    /// An entity, or a prefix with `*` at its end; default all.
    #[serde(default)]
    pub entity: Option<String>,
    /// The dossier scope.
    #[serde(default)]
    pub dossier: Option<String>,
    /// The thread scope.
    #[serde(default)]
    pub thread: Option<i64>,
    /// The first point: a ref, and a valid time (ms); default HEAD and now.
    #[serde(default)]
    pub from_ref: Option<String>,
    #[serde(default)]
    pub from_time: Option<i64>,
    /// The second point.
    #[serde(default)]
    pub to_ref: Option<String>,
    #[serde(default)]
    pub to_time: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct OpsListIn {
    /// Only the operations of this group (`fan`, `thread`); default all.
    #[serde(default)]
    pub group: Option<String>,
}

/// One operation of the catalog.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OpEntry {
    /// Its dotted name (`fan.curve.set`): the CLI's words, the `op` of `legionctl_run`, the button's `op`.
    pub name: String,
    /// The first word of its name.
    pub group: String,
    pub class: crate::Class,
    /// The first sentence of its help.
    pub summary: String,
    /// It shows its plan with `dry_run` before it changes anything.
    pub dry_run: bool,
    /// The fields of its form.
    pub fields: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AskIn {
    /// What to see, in your words ("why is the CPU hot"), or the change of the view `--view` ("add the fan curve").
    pub intent: String,
    /// The view to change: the answer is its next version.
    #[serde(default)]
    pub view: Option<String>,
    /// The view's parameters as `name=value` (its data sample is read with them).
    #[serde(default)]
    pub params: Vec<String>,
    /// The agent that composes (a name of `session.acp`; its model from config `views.composer_models`); default
    /// config `app.agent`.
    #[serde(default)]
    pub agent: Option<String>,
    /// The dossier, account and ACP session of the composer's run (as `session run`).
    #[serde(default)]
    pub dossier: Option<String>,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub acp: Option<String>,
    /// Only a stored view answers (its intent, or rlcd's pick): the composer does not run.
    #[serde(default)]
    pub stored: bool,
}

/// What answered an intent: a stored view, rlcd's pick, or the composer's view (or text).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ViewAnswer {
    /// stored, rlcd or composer.
    pub route: String,
    /// The view that answers; none when the composer answered with text only, or its view failed its check.
    pub view: Option<String>,
    /// The composer's words for the user (the view's spec taken out).
    pub text: String,
    /// The composer's view failed `views check`: why. Nothing was saved.
    pub error: Option<String>,
    /// The view with its data (the terminal form prints it).
    pub data: Option<crate::ViewData>,
    /// The composer's run: its session and ACP session (the chat bar goes on in it).
    pub run: Option<RunOutput>,
    /// Each step that ran, in order, with what it cost (also the event `views.ask`).
    pub steps: Vec<AskStep>,
}

/// One step of an ask: stored (the intent word for word), rlcd (a pick among the stored intents) or composer.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct AskStep {
    /// stored, rlcd or composer.
    pub step: String,
    /// answered, none (it ran and found no view) or failed.
    pub outcome: String,
    /// Tokens sent: rlcd's count, the composer's request as an estimate (4 characters each).
    pub tokens_in: u64,
    /// Tokens of the composer's answer (an estimate); rlcd answers no text.
    pub tokens_out: u64,
    pub ms: u64,
    /// Why it failed, or why it found none.
    pub note: Option<String>,
    /// The account the composer ran on: the chat bar's pick, else the current login's name.
    #[serde(default)]
    pub account: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ViewPartsIn {
    /// What the parts are for, in words: the parts that fit it come first, and only those that share a word.
    #[serde(default)]
    pub intent: Option<String>,
}

/// A part of the library: a stored view with one widget and one source, which a stack places by name and version.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PartInfo {
    pub name: String,
    /// The version that shows now (0: the original); a stack pins one with `{"name": .., "version": ..}`.
    pub version: u32,
    pub title: String,
    pub widget: ViewWidget,
    /// The read operation and the path of its answer.
    pub op: String,
    pub path: String,
}

/// The cost of one step of `ask` over the asks in the event log.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AskCost {
    /// stored, rlcd or composer.
    pub step: String,
    /// The account the composer ran on (none: the step runs no agent, or an ask from before it was kept).
    pub account: Option<String>,
    /// The asks in which it ran, it answered, and it failed.
    pub runs: u64,
    pub answered: u64,
    pub failed: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// The median and the longest time of a run.
    pub ms_median: u64,
    pub ms_max: u64,
}
