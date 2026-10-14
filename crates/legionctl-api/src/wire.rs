//! The wire types of the domain values that operations return or take (DESIGN/03, decided 2026-10-13): each is its
//! own type here, made from the domain type field by field, so a change inside `legionctl-core` or
//! `legionctl-machine` changes the wire only through this file (and `docs/api/schema.json` shows it). Wire names
//! follow one vocabulary: `*_at` for times (seconds since the epoch, `at_ms` in milliseconds), `*_ms`/`*_s` for
//! durations, `*_bytes` for sizes.

use std::collections::BTreeMap;
use std::path::PathBuf;

use legionctl_core as core;
use legionctl_machine as machine;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A domain value as its wire type (the enums that inputs take convert back with `From`).
pub trait IntoWire<W> {
    fn into_wire(self) -> W;
}

macro_rules! same {
    ($($t:ty),*) => {$(
        impl IntoWire<$t> for $t {
            fn into_wire(self) -> $t {
                self
            }
        }
    )*};
}
same!(
    String,
    bool,
    i64,
    u64,
    u32,
    u16,
    u8,
    usize,
    f64,
    PathBuf,
    Value,
    serde_json::Map<String, Value>
);

impl<T: IntoWire<W>, W> IntoWire<Vec<W>> for Vec<T> {
    fn into_wire(self) -> Vec<W> {
        self.into_iter().map(IntoWire::into_wire).collect()
    }
}

impl<T: IntoWire<W>, W> IntoWire<Option<W>> for Option<T> {
    fn into_wire(self) -> Option<W> {
        self.map(IntoWire::into_wire)
    }
}

impl<T: IntoWire<W>, W> IntoWire<BTreeMap<String, W>> for BTreeMap<String, T> {
    fn into_wire(self) -> BTreeMap<String, W> {
        self.into_iter().map(|(k, v)| (k, v.into_wire())).collect()
    }
}

impl<A: IntoWire<X>, B: IntoWire<Y>, X, Y> IntoWire<(X, Y)> for (A, B) {
    fn into_wire(self) -> (X, Y) {
        (self.0.into_wire(), self.1.into_wire())
    }
}

/// `struct Wire from domain::Type { field: Type, wire_name: Type = domain_name, ... }`: the wire struct and its
/// conversion; each field converts through [`IntoWire`].
macro_rules! wire {
    ($(
        $(#[$meta:meta])*
        struct $W:ident from $D:path {
            $( $(#[$fmeta:meta])* $f:ident: $t:ty $(= $src:ident)? ),* $(,)?
        }
    )*) => {$(
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
        pub struct $W {
            $( $(#[$fmeta])* pub $f: $t, )*
        }

        impl IntoWire<$W> for $D {
            fn into_wire(self) -> $W {
                let domain = self;
                $W { $( $f: wire!(@field domain, $f $(, $src)?).into_wire(), )* }
            }
        }
    )*};
    (@field $d:ident, $f:ident) => { $d.$f };
    (@field $d:ident, $f:ident, $src:ident) => { $d.$src };
}

/// `enum Wire from domain::Type as "case" { A, B }`: the wire enum, its conversion, and `From` back for inputs.
macro_rules! wire_enum {
    ($(
        $(#[$meta:meta])*
        enum $W:ident from $D:path as $case:literal { $( $(#[$vmeta:meta])* $v:ident ),* $(,)? }
    )*) => {$(
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
        #[serde(rename_all = $case)]
        pub enum $W {
            $( $(#[$vmeta])* $v, )*
        }

        impl IntoWire<$W> for $D {
            fn into_wire(self) -> $W {
                match self { $( <$D>::$v => $W::$v, )* }
            }
        }

        impl From<$W> for $D {
            fn from(wire: $W) -> $D {
                match wire { $( $W::$v => <$D>::$v, )* }
            }
        }

        impl std::fmt::Display for $W {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                // the wire name: serde's rename_all of this enum
                let name = serde_json::to_value(self).map_err(|_| std::fmt::Error)?;
                f.write_str(name.as_str().unwrap_or_default())
            }
        }
    )*};
}

wire_enum! {
    /// Where a session runs.
    enum Inside from core::session::Inside as "lowercase" {
        /// On this machine, in a bwrap sandbox.
        Host,
        /// In a container of the repository's agent image (`legionctl workspace pack`).
        Container,
        /// In a VM of the repository's disk (`legionctl workspace pack --as vm`), run by `legionctl vm` in the scope.
        Vm,
    }
    /// Hook event: session-start, prompt or stop.
    enum Event from core::reminders::Event as "kebab-case" {
        /// Once, when a session starts.
        SessionStart,
        /// With every user prompt. Keep these few and short: they cost tokens on each turn.
        Prompt,
        /// When the agent is about to end its reply; what runs here goes to the agent, which continues before the user
        /// sees the reply.
        Stop,
    }
    /// How a dossier edit changes its anchored lines.
    enum EditMode from core::dossier::EditMode as "kebab-case" {
        /// Replace the lines from start to end (inclusive) with the content; empty content deletes them.
        Replace,
        /// Insert the content after the start line.
        InsertAfter,
        /// Delete the lines from start to end.
        Delete,
    }
    /// Where a package comes from.
    enum PackageKind from core::packages::PackageKind as "lowercase" {
        Npm,
        Pypi,
        Cargo,
        Git,
        /// A local file or folder, copied in.
        Path,
    }
    /// The workload of a bench or tune step.
    enum Scenario from machine::bench::Scenario as "lowercase" {
        Cpu,
        Gpu,
        Mixed,
    }
    /// Which GPU drives the built-in panel.
    enum GpuMode from machine::gpu::GpuMode as "lowercase" {
        /// The panel is on the iGPU.
        Hybrid,
        /// The panel is on NVIDIA.
        Discrete,
        Unknown,
    }
    /// The GPU Mutter draws the desktop on.
    enum DesktopGpu from machine::gpu::DesktopGpu as "lowercase" {
        Nvidia,
        Intel,
        /// No rule: Mutter picks the GPU of the built-in panel.
        Auto,
    }
    /// What legionctl does with a knob: set it, follow the firmware, or only read it.
    enum KnobPolicy from machine::knobs::KnobPolicy as "lowercase" {
        Enforce,
        Follow,
        Observe,
    }
    /// Where a configset layer is, lowest first.
    enum LayerFolder from machine::configset::Folder as "lowercase" {
        /// The profile's layers (`/usr/share/legionctl/configsets/<profile id>/`): disable them, do not change them.
        Shipped,
        /// Made by legionctl (`tuned`, from `legionctl tune`): off until `legionctl rules enable`.
        Generated,
        /// The system defaults (`/etc/legionctl/configsets/`); can lock knobs against user layers.
        System,
        /// Yours (`~/.config/legionctl/configsets/`), when you are the user in front.
        User,
    }
    /// How busy the machine is.
    enum Level from machine::load::Level as "lowercase" {
        Idle,
        Normal,
        Busy,
        Overloaded,
    }
}

wire! {
    struct DossierInfo from core::dossier::DossierInfo {
        name: String,
        has_plan: bool,
    }

    /// Which session holds a dossier.
    struct Lock from core::dossier::Lock {
        dossier: String,
        session: String,
        /// What bound it: hook (a Claude Code session), an agent name (a legionctl session), cli or mcp.
        tool: String,
        /// Seconds since the epoch.
        updated_at: i64 = updated,
        /// No longer live: the next session that uses the dossier takes it.
        stale: bool,
    }

    struct ConfigFile from core::agents::ConfigFile {
        agent: String,
        scope: String,
        path: PathBuf,
        exists: bool,
        /// None when the file is missing; otherwise whether it parses.
        valid: Option<bool>,
        error: Option<String>,
        /// Top-level keys or tables, e.g. permissions, hooks, `mcp_servers`.
        sections: Vec<String>,
    }

    /// What an edit did (or would do).
    struct Edit from core::agents::Edit {
        path: PathBuf,
        /// Only the changed lines, `-` before and `+` after.
        changes: Vec<String>,
        /// Where the previous file was copied; none for a dry run or a new file.
        backup: Option<PathBuf>,
        warnings: Vec<String>,
        dry_run: bool,
    }

    /// Tokens of one agent on one day with one model.
    struct Usage from core::agents::Usage {
        agent: String,
        /// YYYY-MM-DD (UTC).
        day: String,
        model: String,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
    }

    struct UsageReport from core::agents::UsageReport {
        rows: Vec<Usage>,
        /// Codex's own rate-limit windows from its latest session: name -> percent used.
        codex_limits: BTreeMap<String, f64>,
    }

    /// One config file and the layer it belongs to.
    struct Source from core::config::Source {
        layer: String,
        /// `built in` for the defaults.
        file: String,
    }

    /// A value that a file set for a key.
    struct Setting from core::config::Setting {
        source: Source,
        value: Value,
    }

    /// What `legionctl config explain` shows for one key.
    struct Explanation from core::config::Explanation {
        key: String,
        /// The merged value; null when no layer sets it.
        value: Value,
        locked: bool,
        /// Every file that set this key or a key below it, lowest first.
        settings: Vec<(String, Setting)>,
    }

    /// A layer, where its files are, and which of them exist.
    struct ConfigLayer from core::config::Layer {
        name: String,
        dir: Option<String>,
        files: Vec<String>,
    }

    /// One entry of the event log: who did what, when.
    struct EventRow from core::db::EventRow {
        id: i64,
        /// Seconds since the epoch.
        at: i64 = ts,
        actor: String,
        kind: String,
        payload: Value,
    }

    /// One call through the model gateway.
    struct GatewayUsage from core::db::GatewayUsage {
        id: i64,
        /// Seconds since the epoch.
        at: i64 = ts,
        account: String,
        model: String,
        tokens_in: i64,
        tokens_out: i64,
        /// Replayed from the cache: no provider call.
        cached: bool,
        /// The HTTP status returned to the caller.
        status: u16,
        ms: i64,
        /// From `gateway.prices`; 0 for a replay or a model without a price.
        cost_usd: f64,
    }

    struct Node from core::graph::Node {
        /// `plan#todo-3`, an artifact's front-matter id, or its path without `.md` (`MEMORY/03-workspace`).
        id: String,
        /// The plan section of a row (`ToDo`), or the artifact's class (`EVIDENCE/validation`).
        kind: String,
        /// The file it comes from, relative to the dossier.
        file: String,
        status: Option<String>,
        title: String,
    }

    struct Edge from core::graph::Edge {

        from: String,

        to: String,

        /// supports, supersedes, code, refers-to.
        kind: String,

    }

    /// A node's status changed in a write through legionctl (null: the node did not exist or had no status).
    struct StatusChange from core::graph::StatusChange {
        /// Seconds since the epoch.
        at: i64 = ts,
        session: String,
        node: String,
        from: Option<String>,
        to: Option<String>,
    }

    struct Graph from core::graph::Graph {
        nodes: Vec<Node>,
        edges: Vec<Edge>,
        /// The status changes of the node asked for (`graph --node`), oldest first.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        history: Vec<StatusChange>,
    }

    struct Action from core::hooks::Action {
        id: String,
        builtin: bool,
        /// Description for built-ins; the shell command for user-defined actions.
        detail: String,
        timeout_s: u32,
    }

    struct Binding from core::hooks::Binding {

        event: Event,

        action: String,

        position: usize,

    }

    struct Build from core::images::Build {

        context: String,

        dockerfile: String,

        /// The last stage, which `docker build` builds by default.
        target: Option<String>,

    }

    struct Mount from core::images::Mount {
        /// bind (a host path, relative to the repository when inside it) or volume (a named volume).
        kind: String,
        source: String,
        target: String,
        read_only: bool,
    }

    struct Resources from core::images::Resources {
        cpus: Option<f64>,
        memory_bytes: Option<u64>,
        /// The service reserves a GPU.
        gpu: bool,
    }

    struct Service from core::images::Service {
        /// None for a prebuilt image from the compose file.
        build: Option<Build>,
        /// The prebuilt image a compose service runs (pulled, not built).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image: Option<String>,
        /// run and health, when the Dockerfile (or the compose health check) says.
        entrypoints: BTreeMap<String, Vec<String>>,
        /// `EXPOSE`d ports, or the compose file's: `<published>:<port>` when published on the host.
        ports: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mounts: Vec<Mount>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resources: Option<Resources>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        depends_on: Vec<String>,
    }

    struct CacheEntry from core::images::CacheEntry {

        /// External base images (stages that build on other stages are not bases).
        bases: Vec<String>,

        /// Lockfiles in the build context: a change rebuilds the layers that copy them.
        lockfiles: Vec<String>,

    }

    struct Scan from core::images::Scan {
        services: BTreeMap<String, Service>,
        cache: BTreeMap<String, CacheEntry>,
        /// The repository's compose file, which `images run` passes before legionctl's override.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compose_file: Option<String>,
    }

    /// What the store recorded at fetch time.
    struct Manifest from core::packages::Manifest {
        name: String,
        kind: PackageKind,
        source: String,
        version: String,
        /// BLAKE2b-256 of the installed tree, hex.
        hash: String,
        /// Seconds since the epoch.
        fetched_at: i64 = fetched,
    }

    struct PackageState from core::packages::PackageState {
        name: String,
        kind: PackageKind,
        source: String,
        version: String,
        dir: String,
        /// None: not fetched yet.
        manifest: Option<Manifest>,
        /// The tree still has the recorded hash.
        intact: Option<bool>,
    }

    struct Reminder from core::reminders::Reminder {
        id: String,
        text: String,
        event: Event,
        enabled: bool,
        /// Set by the built-in defaults; it can be turned off, not removed.
        builtin: bool,
    }

    /// How the client starts a session in its terminal: `scope` + `sandbox` + `agent`, in that order.
    struct Launch from core::session::Launch {
        id: String,
        /// `systemd-run --user --scope ...`: the session's cgroup, for `session stop` and limits.
        scope: Vec<String>,
        /// `bwrap ...` (the caller adds the git gate: its own legionctl over `/usr/bin/git`), `docker run ...` of the
        /// agent image, whose `/usr/bin/git` is the gate already, or `vm <spec> --`, which the caller runs with its own
        /// legionctl.
        sandbox: Vec<String>,
        inside: Inside,
        agent: Vec<String>,
        /// Where the real git is inside the sandbox, for the gate.
        real_git: PathBuf,
        /// The session's workspace: the agent's working folder.
        workdir: PathBuf,
        /// The session's MCP servers (`mcp.json`): legionctl's own and each one behind the proxy.
        mcp: Value,
        /// What the session uses (its dossier, new or not; its workspace, branch and size) and the commands that change
        /// them; the client shows them before the agent starts.
        lines: Vec<String>,
    }

    /// One session that legionctl started.
    struct SessionRecord from core::session::SessionRecord {
        id: String,
        agent: String,
        dossier: String,
        /// The workspace the agent works in.
        workdir: PathBuf,
        /// Seconds since the epoch.
        started_at: i64 = started,
        /// The account profile (`agents accounts add`), if one was chosen.
        account: Option<String>,
        inside: Inside,
    }

    struct SetupItem from core::setup::Item {

        /// e.g. "ctx skill", "ctx mcp", "legionctl mcp", "legionctl skill ste100"
        what: String,

        agent: String,

        /// "current" when in place; otherwise what is wrong (missing, outdated, `invalid_config`, ...)
        status: String,

        path: String,

    }

    struct SetupReport from core::setup::Report {

        ctx_version: Option<String>,

        items: Vec<SetupItem>,

    }

    struct SkillInfo from core::skills::SkillInfo {
        /// Skill name; Claude Code loads it as `legionctl:<name>`.
        name: String,
        description: String,
        /// Path inside the installed Claude Code plugin, when the plugin is installed.
        claude: Option<PathBuf>,
        /// Codex link (~/.codex/skills/legionctl-<name>) and whether it resolves.
        codex: PathBuf,
        codex_linked: bool,
        /// Reminder that keeps the skill in force, and its state.
        reminder: Option<String>,
        reminder_on: Option<bool>,
    }

    /// One open workspace.
    struct Workspace from core::workspace::Workspace {
        dossier: String,
        /// The repository's main checkout; for a plain folder, the folder.
        repo: PathBuf,
        /// The operating directory: work here, not in the main checkout.
        path: PathBuf,
        /// The active branch: `legionctl/<dossier>` or `legionctl/<dossier>--<topic>`.
        branch: String,
        /// The ref the branch started from and merges into.
        base: String,
    }

    /// One mutation and the tests that failed with it.
    struct Mutant from core::codetests::Mutant {
        file: String,
        line: u32,
        description: String,
        /// None: the mutant survived.
        killed_by: Vec<String>,
        /// `tool` (cargo-mutants) or `agent` (a hand-made mutation).
        source: String,
        /// HEAD when it was recorded.
        git_ref: String,
        recorded_at: i64 = at,
    }

    /// One result of a test in a run.
    struct TestRun from core::codetests::TestRun {
        /// The source of the test binary.
        file: String,
        /// ok, failed or ignored.
        result: String,
        git_ref: String,
        job: Option<i64>,
        ran_at: i64 = at,
    }

    /// A test that the gate let through without evidence.
    struct TestAccept from core::codetests::Accept {
        reason: String,
        session: String,
        accepted_at: i64 = at,
    }

    /// A test with its last run and its kills.
    struct TestRow from core::codetests::TestRow {
        name: String,
        file: String,
        result: String,
        git_ref: String,
        ran_at: i64 = at,
        /// The recorded mutants it killed.
        kills: u32,
    }

    /// Everything recorded for one test.
    struct TestDetail from core::codetests::TestDetail {
        name: String,
        /// Newest first.
        runs: Vec<TestRun>,
        mutants: Vec<Mutant>,
        accepts: Vec<TestAccept>,
    }

    /// The review of one new or changed test.
    struct ReviewedTest from core::codetests::ReviewedTest {
        name: String,
        file: String,
        assertions: u32,
        kills: u32,
        /// In a `tests/` folder: it reaches the code through the crate's boundary.
        boundary: bool,
        /// A test that kills each mutant this one kills.
        duplicate_of: Option<String>,
        /// Hard signals: the checkpoint gate refuses the test for these.
        refused: Vec<String>,
        /// Soft signals.
        notes: Vec<String>,
        /// The reason of `--accept-test`, when the test was let through.
        accepted: Option<String>,
    }

    /// The new and changed tests of a change set.
    struct TestReview from core::codetests::TestReview {
        tests: Vec<ReviewedTest>,
    }

    /// Git state of a workspace and what drifted from the record.
    struct WorkspaceStatus from core::workspace::WorkspaceStatus {
        #[serde(flatten)]
        workspace: Workspace,
        /// Commits on the branch that the base does not have, and the other way round.
        ahead: u32,
        behind: u32,
        staged: u32,
        unstaged: u32,
        untracked: u32,
        conflicted: u32,
        /// What differs from the record: a branch switched outside legionctl, a missing worktree.
        drift: Vec<String>,
        /// A plain folder's changes, one per line (A, M, D, R and the path); empty for a repository.
        files: Vec<String>,
    }

    /// The samples of a run, summed up.
    struct BenchSummary from machine::bench::Summary {
        samples: usize,
        package_w_mean: Option<f64>,
        package_w_max: Option<f64>,
        cpu_temp_max: Option<f64>,
        cpu_mhz_mean: Option<f64>,
        gpu_w_mean: Option<f64>,
        gpu_temp_max: Option<f64>,
        fan_rpm_max: Option<u64>,
        /// The worst run-queue latency p99 of any sample and CPU class (µs, the eBPF sensor).
        runq_p99_us_max: Option<u64>,
        /// Throttle reasons active in any sample.
        throttle: Vec<String>,
    }

    /// What a run stores in the event log (`bench.result`).
    struct BenchResult from machine::bench::BenchResult {
        scenario: Scenario,
        label: Option<String>,
        seconds: u32,
        scores: BTreeMap<String, f64>,
        /// Failed stressors, GPU computation errors, a workload that exited with an error.
        problems: Vec<String>,
        summary: BenchSummary,
        /// Every knob's value when the run started.
        knobs: BTreeMap<String, Option<String>>,
        /// The package of the kernel it ran on (`linux-cachyos`); none for a run before kernels were recorded.
        kernel: Option<String>,
        /// The release of that kernel (`7.2.9-2-cachyos`).
        release: Option<String>,
    }

    struct Output from machine::gpu::Output {

        connector: String,

        card: String,

        driver: String,

        connected: bool,

    }

    /// The battery now.
    struct Battery from machine::battery::Battery {
        /// standard, conservation or rapid.
        mode: Option<String>,
        status: Option<String>,
        capacity_pct: Option<u32>,
        cycle_count: Option<u32>,
        energy_full_wh: Option<f64>,
        energy_full_design_wh: Option<f64>,
        /// Full vs design capacity, in percent.
        health_pct: Option<f64>,
        /// Watts into or out of the battery.
        power_w: Option<f64>,
        /// USB ports give power while the laptop is off.
        usb_charging: Option<bool>,
    }

    /// A firmware boot entry.
    struct BootEntry from machine::boot::Entry {
        /// `0003`
        id: String,
        label: String,
    }

    /// How the computer starts.
    struct BootState from machine::boot::BootState {
        /// The ESP's mount point.
        esp: Option<String>,
        entries: Vec<BootEntry>,
        order: Vec<String>,
        /// This start's entry.
        current: Option<String>,
        /// The next start's entry, once.
        next: Option<String>,
        /// legionctl's entry, when there is one.
        ours: Option<String>,
        /// Kernels in /boot.
        kernels: Vec<String>,
        /// legionctl's kernel images on the ESP.
        ukis: Vec<String>,
    }

    /// The login screen.
    struct GreeterState from machine::greeter::GreeterState {
        /// greetd's configuration is legionctl's.
        installed: bool,
        /// The display manager that starts at boot.
        display_manager: Option<String>,
        /// The greetd configuration before legionctl's is kept.
        kept: bool,
        /// What the greeter needs and is not installed.
        missing: Vec<String>,
    }

    /// An installed kernel.
    struct KernelInfo from machine::kernel::Installed {
        /// The package (`linux-cachyos-bore-lto`).
        package: String,
        /// `7.2.9-2-cachyos-bore-lto`
        release: String,
        /// `bore` or `eevdf`, and `lto`; empty without the kernel headers.
        flavour: Vec<String>,
        /// legionctl's signed image of it.
        image: Option<String>,
        /// Its command line; null until the boot takeover writes it.
        cmdline: Option<String>,
        /// `nvidia/580.178.04: installed`, `legionctl-wmi: not built`.
        dkms: Vec<String>,
        running: bool,
        /// legionctl's boot entry starts it when no one chooses.
        default: bool,
    }

    /// The installed kernels.
    struct Kernels from machine::kernel::Kernels {
        /// The boot takeover is installed: `kernel default` and `kernel try` need it.
        takeover: bool,
        kernels: Vec<KernelInfo>,
    }

    /// A device that can go to a VM (VFIO): a discrete GPU or an `NVMe` controller.
    struct VfioDevice from machine::vfio::Candidate {
        /// PCI address (`0000:01:00.0`).
        device: String,
        /// gpu or nvme.
        kind: String,
        /// PCI vendor and device ids (`10de:27e0`).
        ids: String,
        /// Its IOMMU group; a VM gets the whole group.
        group: String,
        members: Vec<String>,
        /// Its driver now (`vfio-pci` while a VM has it).
        driver: Option<String>,
        /// What on the host uses the group now; it can go to a VM only when this is empty.
        users: Vec<String>,
    }

    /// One layer that sets a knob now (the "because" chain); the last one wins.
    struct Link from machine::configset::Link {
        /// `<folder>/<name>` (`shipped/20-ac`).
        layer: String,
        /// Its conditions (`power ac`, `always`).
        when: String,
        value: Value,
    }

    /// A configset layer: `<order>-<name>.jsonc`, `{ "when": {...}, "set": {...} }`.
    struct RuleLayer from machine::configset::Layer {
        /// `<order>-<name>`.
        name: String,
        folder: LayerFolder,
        file: String,
        /// `power` (ac, battery), `fnq` (the Fn+Q mode), `workload` (game, build, vm); empty: always.
        when: BTreeMap<String, String>,
        /// Knob id -> value.
        set: serde_json::Map<String, Value>,
        /// Knobs that user layers cannot set (system layers only).
        locked: Vec<String>,
        /// Why it is not used; null: it is used while its conditions hold.
        off: Option<String>,
        /// It is not used because it fails a check (a doctor finding).
        failed: bool,
    }

    struct Knob from machine::knobs::Knob {
        /// `platform_profile`, `cpu.governor`, `sysctl.vm.swappiness`, `firmware.ppt_pl1_spl`, ...
        id: String,
        /// The file; `*` for a per-CPU knob (the value sums the CPUs up).
        path: String,
        value: Option<String>,
        /// What the profile wants now; `None` for a knob legionctl does not set.
        desired: Option<String>,
        min: Option<String>,
        max: Option<String>,
        choices: Vec<String>,
        policy: KnobPolicy,
        held: Option<String>,
        /// Every layer that sets it now, in merge order; the last one wins, unless it is held.
        chain: Vec<Link>,
    }

    struct Pressure from machine::load::Pressure {

        /// Percent of the last 10 s in which at least one task stalled on this resource.
        some_avg10: f64,

        /// Percent of the last 10 s in which all non-idle tasks stalled (not reported for CPU on every kernel).
        full_avg10: Option<f64>,

    }

    struct LoadState from machine::load::Load {
        level: Level,
        reasons: Vec<String>,
        load1: f64,
        cpus: u32,
        cpu: Option<Pressure>,
        memory: Option<Pressure>,
        io: Option<Pressure>,
        mem_available_bytes: u64,
        mem_total_bytes: u64,
        /// With I/O pressure while no disk works: the cgroup that stalls most, and its pressure. Not counted in `level`.
        io_stall_source: Option<String>,
    }

    struct Capability from machine::profile::Capability {
        id: String,
        name: String,
        status: Option<String>,
        current_owner: Option<String>,
        planned_owner: Option<String>,
        notes: Option<String>,
    }

    struct DmiMismatch from machine::snapshot::DmiMismatch {

        key: String,

        expected: String,

        actual: Option<String>,

    }

    struct RaplConstraint from machine::snapshot::RaplConstraint {
        name: String,
        limit_w: Option<f64>,
        window_us: Option<u64>,
    }

    struct RaplZone from machine::snapshot::RaplZone {

        zone: String,

        constraints: Vec<RaplConstraint>,

    }

    struct UnitState from machine::snapshot::UnitState {

        unit: String,

        active: String,

        enabled: String,

    }

    struct Snapshot from machine::snapshot::Snapshot {
        profile_id: String,
        /// Empty when the machine matches the profile's DMI identity.
        dmi_mismatches: Vec<DmiMismatch>,
        /// `bios_version`, `bios_release`, `ec_firmware_release`, `board_name`.
        firmware_versions: BTreeMap<String, Option<String>>,
        /// "ac" (the barrel adapter), "usb-c" (USB-C power only; the battery policy applies), "battery" or "unknown".
        power_source: String,
        /// Firmware thermal mode: low-power, balanced, performance, max-power, custom.
        platform_profile: Option<String>,
        platform_profile_choices: Vec<String>,
        /// Set while legionctl follows a mode chosen with Fn+Q.
        fn_q_override: Option<String>,
        /// `no_turbo`, `max_perf_pct` (`intel_pstate`) and `ignore_ppc` (processor).
        cpu_settings: BTreeMap<String, Option<String>>,
        /// CPU frequency governors and how many policies use each.
        governors: BTreeMap<String, usize>,
        energy_performance_preference: BTreeMap<String, usize>,
        /// Lenovo firmware limits (W / °C), e.g. `ppt_pl1_spl`, `ppt_pl2_sppt`, `cpu_temp`.
        firmware_limits: BTreeMap<String, String>,
        /// Package RAPL zones (MSR and MMIO) with their limits; read for watts, never written.
        rapl: Vec<RaplZone>,
        nvidia_power_limit_w: Option<String>,
        /// Firmware fan RPM (needs root).
        fan_rpm: Option<BTreeMap<String, u64>>,
        /// Throttle reasons logged since boot (needs root).
        throttle_reasons: Option<Vec<String>>,
        package_temp_c: Option<f64>,
        outputs: Vec<Output>,
        gpu_mode: GpuMode,
        /// GPU GNOME draws the desktop on.
        desktop_gpu: DesktopGpu,
        units: Vec<UnitState>,
        /// Modules that matter on this machine, and whether each is loaded.
        modules: BTreeMap<String, bool>,
        load: LoadState,
    }

    struct GpuSample from machine::telemetry::GpuSample {
        /// PCI runtime power state: active, suspended, ...; the values below only when active.
        state: String,
        power_w: Option<f64>,
        temp_c: Option<f64>,
        util_percent: Option<f64>,
        clock_mhz: Option<f64>,
    }

    /// Run-queue latency over a sample window: how long woken tasks waited for a CPU. Log2 resolution: a percentile is
    /// the upper bound of its bucket.
    struct Latency from machine::telemetry::Latency {
        count: u64,
        p50_us: u64,
        p99_us: u64,
    }

    struct Sample from machine::telemetry::Sample {
        /// Milliseconds since the epoch at the end of the window.
        at_ms: u64 = ts_ms,
        /// Package power over the sample window (RAPL energy).
        package_w: Option<f64>,
        cpu_temp_c: Option<f64>,
        /// Mean current clock over all CPUs.
        cpu_mhz: Option<f64>,
        gpu: Option<GpuSample>,
        cpu_pressure: Option<Pressure>,
        memory_pressure: Option<Pressure>,
        io_pressure: Option<Pressure>,
        /// Run-queue latency in the window per CPU class (the daemon's eBPF sensor).
        runq_latency: Option<BTreeMap<String, Latency>>,
        /// Every hwmon channel.
        hwmon: Vec<Channel>,
        /// The hwmon devices not read: their device sleeps.
        hwmon_asleep: Vec<String>,
        #[serde(flatten)]
        root: RootSample,
    }

    /// One hwmon channel (`temp1_input` of `hwmon4`), in its unit.
    struct Channel from machine::telemetry::Channel {
        /// The hwmon device and its name: `hwmon4 coretemp`.
        device: String,
        /// The channel's label, else its name (`temp1`).
        name: String,
        value: f64,
        /// °C, RPM, W, V, A or MHz.
        unit: String,
    }

    /// One row of `legionctl sensors`: a number with its unit, or a state as text.
    struct Reading from machine::sensors::Reading {
        /// Where it comes from: `hwmon4 coretemp`, `RAPL`, `nvidia-smi`, `firmware acpi_call`, `eBPF runq`, ...
        source: String,
        name: String,
        value: Option<f64>,
        /// A state (`suspended`, `thermal, power limit`); a number that cannot be read is `unknown` and why.
        text: Option<String>,
        unit: String,
        /// The values of this reading in the earlier samples of the telemetry ring, oldest first, then this one.
        history: Vec<f64>,
    }

    /// What only root can read; `legionctl-system` answers it over D-Bus (`Sample`).
    struct RootSample from machine::telemetry::RootSample {
        fan_rpm: Option<BTreeMap<String, u64>>,
        /// Throttle reasons active now.
        throttle_now: Option<Vec<String>>,
        /// Throttle reasons logged since boot (the MSR's sticky bits).
        throttle_logged: Option<Vec<String>>,
        /// The eBPF sensor's cumulative run-queue wait histogram per CPU class (bucket i: 2^i to 2^(i+1) µs). A sample
        /// turns two of these into `runq_latency`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        runq: Option<BTreeMap<String, Vec<u64>>>,
        /// Why the eBPF sensor does not run (no permission, the verifier's message).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        runq_error: Option<String>,
    }

    struct TuneStep from machine::tune::TuneStep {
        value: u32,
        scores: BTreeMap<String, f64>,
        problems: Vec<String>,
        summary: BenchSummary,
        /// Throttle reasons in the second half of the step.
        steady_throttle: Vec<String>,
    }

    struct TuneReport from machine::tune::TuneReport {
        attribute: String,
        scenario: Scenario,
        /// The value before the sweep; it is written back at the end.
        original: String,
        steps: Vec<TuneStep>,
        recommended: Option<u32>,
        /// Why this value, or why none.
        reason: String,
        /// Why the sweep stopped early.
        aborted: Option<String>,
        /// The results file (machine path).
        file: String,
    }

    /// The fan controls (`legionctl-system` reads the firmware's table).
    struct FanState from machine::fan::FanState {
        /// The firmware mode now; the fan curve applies in `custom` only.
        mode: Option<String>,
        /// The firmware's fan table now (levels 1-10 at points 1-10); null when it cannot be read.
        curve: Option<Vec<u8>>,
        /// Why the table cannot be read.
        curve_error: Option<String>,
        /// Your curve: written again after resume and at each entry to custom mode; null when you have none.
        chosen: Option<Vec<u8>>,
        /// The profile's curve, used when you have none.
        default_curve: Vec<u8>,
        /// Fan max mode as `legionctl-system` set it (the firmware does not report it).
        max: bool,
        /// RPM of fan 1 at each level 1-10.
        fan1_rpm: Vec<u32>,
        /// RPM of fan 2 at each level 1-10.
        fan2_rpm: Vec<u32>,
        /// The profile allows the EC fan lock.
        ec_fan_lock_verified: bool,
    }

    struct Job from core::db::Job {
        id: i64,
        kind: String,
        input: Value,
        state: String,
        attempts: u32,
        created_at: i64 = created,
        updated_at: i64 = updated,
        result: Option<Value>,
        error: Option<String>,
    }

    struct WindowsState from machine::windows::WindowsState {
        /// The firmware entry that starts Windows (`0004`), if there is one.
        boot_entry: Option<String>,
        boot_entry_label: Option<String>,
        /// A one-time boot the next start uses, if one is set.
        boot_next: Option<String>,
        /// NTFS volumes the kernel reported dirty since boot: Windows was not shut down fully (Fast Startup or
        /// hibernation), so Linux must not write them.
        dirty_ntfs: Vec<String>,
        /// The SYSTEM hive the registry values come from; None when no mounted NTFS volume holds one.
        system_hive: Option<String>,
        /// Windows keeps the hardware clock in UTC (`RealTimeIsUniversal` is 1), as Linux does.
        rtc_utc: Option<bool>,
        /// Fast Startup is on (`HiberbootEnabled` is 1): Windows hibernates at shut down and leaves its volumes dirty.
        fast_startup: Option<bool>,
        /// What needs attention before booting Windows or mounting its volumes.
        hazards: Vec<String>,
    }

    /// A ref of the repository and whether it has a graph.
    struct GraphRef from core::codegraph::GraphRef {
        name: String,
        commit: String,
        tree: String,
        /// Its snapshot exists ("has .graf").
        graph: bool,
    }

    /// One graph snapshot (`.graf/refs/<tree>.db`).
    struct GraphSnapshot from core::codegraph::GraphSnapshot {
        tree: String,
        size_bytes: u64 = bytes,
        modified_at: i64 = modified,
        /// The heads, bases and merge-bases on this tree: prune keeps it whatever its age.
        refs: Vec<String>,
    }

    /// The repository's refs and graph snapshots.
    struct GraphRefs from core::codegraph::GraphRefs {
        /// HEAD and the branches.
        refs: Vec<GraphRef>,
        /// Newest first.
        snapshots: Vec<GraphSnapshot>,
    }

    /// One symbol of a graph.
    struct CodeNode from core::codegraph::CodeNode {
        /// The stable key: the same at each ref (`rust:<crate dir>:<module path>::<name>`, `file:<path>`).
        key: String,
        /// ctx's node id at this tree (it holds byte offsets).
        id: String,
        kind: String,
        file: String,
        line: Option<i64>,
    }

    /// Symbols of the graph of one tree.
    struct CodeNodes from core::codegraph::CodeNodes {
        tree: String,
        nodes: Vec<CodeNode>,
    }

    /// One entity that changed between two refs.
    struct EntityChange from core::codegraph::EntityChange {
        key: String,
        kind: String,
        file: String,
        /// added, removed, modified (other content) or moved (same content, other file). A rename is removed plus
        /// added.
        change: String,
        /// `+a -r` lines for a file; the span in lines for a symbol.
        lines: String,
        /// The commits of the range that touched its lines (its file, for a removed entity).
        commits: Vec<String>,
    }

    /// One commit of a change set and what made it.
    struct CommitRow from core::codegraph::CommitRow {
        commit: String,
        committed_at: i64 = time,
        author: String,
        subject: String,
        /// The plan rows that the message names (`todo-NN`).
        plan_rows: Vec<String>,
        /// The `workspace.checkpoint` event that made it, and its session.
        checkpoint: Option<i64>,
        session: Option<String>,
        /// `ctx blame commit`: the agent session that made it, or ctx's error.
        provenance: Value,
    }

    /// The changes from one ref to the next, at entity and event level.
    struct ChangeSet from core::codegraph::ChangeSet {
        /// BLAKE2b-128 of `<from tree>..<to tree>`.
        id: String,
        from: String,
        from_tree: String,
        to: String,
        to_tree: String,
        entities: Vec<EntityChange>,
        /// Newest first.
        commits: Vec<CommitRow>,
    }

    /// The state of an annotation at one ref.
    struct AnnotationAt from core::codegraph::AnnotationAt {
        #[serde(rename = "ref")]
        rev: String,
        tree: String,
        /// valid (the key with the same content), stale (other content), absent (no such key), no graph.
        state: String,
    }

    /// An annotation on a stable key, and its state at each ref asked for.
    struct Annotation from core::codegraph::Annotation {
        id: i64,
        key: String,
        kind: String,
        value: Value,
        /// The tree and the symbol's content hash when it was written.
        tree: String,
        content_hash: String,
        session: String,
        agent: String,
        created_at: i64 = created,
        at: Vec<AnnotationAt>,
    }
}
