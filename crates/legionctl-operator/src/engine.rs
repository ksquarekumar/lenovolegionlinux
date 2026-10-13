//! The operations of `legionctl-api`, carried out as the user. Machine state is read here; machine changes go to
//! `legionctl-system` over D-Bus, which asks polkit. Every call that is not a read is recorded in the event log.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use legionctl_api::{
    Account, BenchCompare, BenchCompareIn, BenchIn, BenchResult, BenchRun, BenchRuns, Code,
    DossierAddIn, DossierAppendIn, DossierCheckIn, DossierEditIn, DossierFindings, DossierList,
    DossierReadIn, DossierSearchIn, DossierUseIn, DossierWriteIn, DryRunIn, Events, EventsIn,
    GateIn, Guard, GuardIn, HookReply, HookRunIn, InspectIn, KnobDetail, Knobs, Launch, Lines,
    Locks, MonitorIn, RecordIn, Recording, ReplayIn, Sample, Samples, SessionExec, SessionId,
    SessionIdIn, SessionIn, SessionResumeIn, SessionStartIn, SessionState, Sessions, ToolCall,
    ToolCallRow, ToolCalls, ToolCallsIn, TuneIn, TuneReport, WindowsState, coded,
};
use legionctl_api::{
    BranchIn, CapabilityList, Changes, CheckpointIn, CleanupIn, ClientPath, CloseIn, ConfigFiles,
    ConfigView, Ctx, DesktopGpu, Empty, Engine, ExplainIn, Explanation, Findings, GpuPrimaryIn,
    GpuState, HookAddIn, HookBindIn, HookTable, IdIn, LayerList, LoadState, PowerApplyIn,
    PowerSource, ProfileIn, ProjectIn, ReminderAddIn, Reminders, RunEnv, RunEnvIn, SetupApplyIn,
    SetupReport, SkillList, Snapshot, StatusIn, Validation, Workspace, WorkspaceIn,
    WorkspaceOpenIn, WorkspaceStates, Workspaces,
};
use legionctl_core::config::{Dirs, Resolved, resolve};

use crate::system1::System1Proxy;
use legionctl_api::{
    AgentName, AgentSetIn, AgentUsageIn, CoreClass, DossierGraphIn, DossierNudgesIn, Edit,
    GatewayUsage, GatewayUsageIn, Graph, Plugin, PluginInstallIn, SettingsScope, UsageReport,
    VirtCreateIn, VirtTuneIn,
};
use legionctl_api::{
    CacheGetIn, CachePutIn, CacheValue, Manifest, PackageFetchIn, Packages, RunOutput, SessionRunIn,
};
use legionctl_api::{ImagesScanIn, RepoIn, Scan, ServiceState, VirtActIn, VirtStatus};
use legionctl_core::db::Db;
use legionctl_core::dossier::{Dossiers, place_of};
use legionctl_core::packages::Store;
use legionctl_core::reminders::Event;
use legionctl_core::session::Start;
use legionctl_core::workspace::Place;
use legionctl_core::{agents, hooks, reminders, setup, skills};
use legionctl_machine::ledger::Ledger;
use legionctl_machine::policy::{self, Machine, Options, Source};
use legionctl_machine::profile::{self, Profile};
use legionctl_machine::snapshot::Detail;
use legionctl_machine::sysroot::SysRoot;
use legionctl_machine::telemetry::{self, RootSample};
use legionctl_machine::{bench, cleanup, gpu, knobs, load, windows};

pub struct Operator {
    /// Runs the D-Bus calls of machine changes; engine methods run on blocking threads.
    pub runtime: tokio::runtime::Handle,
}

fn has_command(name: &str) -> bool {
    which(name).is_some()
}

fn ppd_current() -> Option<String> {
    let out = Command::new("powerprofilesctl").arg("get").output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn path(p: Option<&ClientPath>) -> Result<Option<&Path>> {
    p.map(ClientPath::get).transpose()
}

fn bench_run_of(e: legionctl_api::EventRow) -> Result<BenchRun> {
    Ok(BenchRun {
        id: e.id,
        ts: e.ts,
        result: serde_json::from_value(e.payload)?,
    })
}

/// The full path of an executable on `PATH`.
pub(crate) fn which(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| {
                m.is_file()
                    && std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0
            })
        })
}

/// `systemctl --user <verb>` on a session's scope (stop, freeze, thaw).
fn scope_control(verb: &str, id: &str, done: &str) -> Result<Lines> {
    let session = dossiers()?.session(id)?;
    let unit = format!("legionctl-session-{}.scope", session.id);
    let out = Command::new("systemctl")
        .args(["--user", verb, &unit])
        .output()?;
    anyhow::ensure!(
        out.status.success(),
        "{verb} {unit}: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(Lines {
        lines: vec![format!("session {} ({}) {done}", session.id, session.agent)],
    })
}

/// The CPU list of a core class: the P-cores, the E-cores or every online CPU.
fn core_cpus(class: Option<CoreClass>) -> Result<Option<String>> {
    let root = SysRoot::from_env();
    let cpus = match class {
        None => return Ok(None),
        Some(CoreClass::All) => root
            .read("/sys/devices/system/cpu/online")
            .context("no /sys/devices/system/cpu/online")?,
        Some(class) => {
            let key = if class == CoreClass::P { "p" } else { "e" };
            legionctl_machine::gpu::core_class_cpus(&root, key).with_context(|| {
                format!("this CPU has no {key}-core class (/sys/devices/cpu_core, cpu_atom)")
            })?
        }
    };
    Ok(Some(cpus.trim().to_string()))
}

/// Bytes from `8589934592`, `8192m` or `8g` (k, m and g are powers of 1024).
fn parse_bytes(text: &str) -> Result<u64> {
    let text = text.trim().to_lowercase();
    let (digits, shift) = match text.strip_suffix(['k', 'm', 'g']) {
        Some(d) => (
            d,
            10 * (1 + "kmg".find(text.chars().last().unwrap_or('k')).unwrap_or(0)),
        ),
        None => (text.as_str(), 0),
    };
    let n: u64 = digits
        .parse()
        .with_context(|| format!("{text} is not a size (bytes, or with k, m or g)"))?;
    n.checked_shl(u32::try_from(shift)?)
        .filter(|b| b >> shift == n)
        .with_context(|| format!("{text} is too large"))
}

/// Operations that change the machine (or record it): their events go into a recording.
const MACHINE_OPS: [&str; 5] = [
    "power.apply",
    "gpu.primary",
    "cleanup",
    "windows.reboot",
    "record",
];

/// The machine, its profile and the Fn+Q ledger, as the planners need them.
struct Plan {
    root: SysRoot,
    profile: Profile,
    ledger: Ledger,
}

impl Plan {
    fn load(profile: Option<&Path>) -> Result<Plan> {
        Plan::at(SysRoot::from_env(), profile)
    }

    /// The machine under `root`; without `profile`, the profile installed there.
    fn at(root: SysRoot, profile: Option<&Path>) -> Result<Plan> {
        let installed = root.path(profile::DEFAULT_PATH);
        Ok(Plan {
            ledger: Ledger::open(&root)?,
            profile: profile::load(Some(profile.unwrap_or(&installed)))?.0,
            root,
        })
    }

    /// What the daemon would write now (`power apply`, auto), what `doctor` finds and the knobs.
    fn decisions(&self, dir: &Path, events: Events) -> Result<Recording> {
        let machine = self.machine();
        Ok(Recording {
            dir: dir.display().to_string(),
            paths: 0,
            plan: dry_run(&machine.apply(None, Options::default())?.1).lines,
            findings: legionctl_machine::doctor::doctor(&machine),
            knobs: knobs::catalog(&machine),
            events,
        })
    }

    fn machine(&self) -> Machine<'_> {
        Machine {
            root: &self.root,
            profile: &self.profile,
            ledger: &self.ledger,
            ppd_current: has_command("powerprofilesctl").then(ppd_current).flatten(),
            has_command: &has_command,
        }
    }
}

fn dry_run(steps: &[policy::Step]) -> Changes {
    Changes {
        lines: policy::grouped(steps),
        dry_run: true,
    }
}

impl Operator {
    /// A machine change through `legionctl-system`; polkit asks the human.
    fn system<T>(&self, call: impl AsyncFnOnce(&System1Proxy<'_>) -> zbus::Result<T>) -> Result<T> {
        self.runtime.block_on(async {
            let conn = zbus::Connection::system()
                .await
                .context("connecting to the system bus")?;
            let proxy = System1Proxy::new(&conn).await?;
            call(&proxy).await.map_err(|e| {
                let code = match &e {
                    zbus::Error::FDO(fdo) => match **fdo {
                        zbus::fdo::Error::AccessDenied(_) => Code::Denied,
                        zbus::fdo::Error::InvalidArgs(_) => Code::InvalidInput,
                        _ => Code::Failed,
                    },
                    _ => Code::Failed,
                };
                coded(code, format!("legionctl-system: {e}"))
            })
        })
    }

    fn hook_table(config: &Resolved) -> HookTable {
        HookTable {
            actions: hooks::actions(config),
            bindings: hooks::bindings(config),
        }
    }
}

fn dossiers() -> Result<Dossiers> {
    Dossiers::open(&config(None)?.config.dossier)
}

/// The agent side of `doctor`: setup gaps, stale dossier bindings, workspaces that drifted from their record.
fn agent_findings() -> Result<Vec<String>> {
    let mut w: Vec<String> = setup::check()
        .needs_attention()
        .iter()
        .map(|i| {
            format!(
                "setup: {} for {} is {}; run legionctl setup apply",
                i.what, i.agent, i.status
            )
        })
        .collect();
    let store = dossiers()?;
    for lock in store.locks()?.iter().filter(|l| l.stale) {
        w.push(format!(
            "dossier {} is bound to session {}, idle past dossier.stale_hours; the next session that uses it takes it over",
            lock.dossier, lock.session
        ));
    }
    for ws in store.workspaces(None)? {
        let status = store.workspace_status(&ws)?;
        for drift in &status.drift {
            w.push(format!("workspace {}: {drift}", ws.path.display()));
        }
        if status.conflicted > 0 {
            w.push(format!(
                "workspace {} has {} conflicted files",
                ws.path.display(),
                status.conflicted
            ));
        }
    }
    Ok(w)
}

fn session(id: &SessionId) -> Result<&str> {
    anyhow::ensure!(
        !id.0.is_empty(),
        "this acts on an agent session's dossier: run it in a session (LEGIONCTL_SESSION or CLAUDE_CODE_SESSION_ID)"
    );
    Ok(&id.0)
}

/// A new or resumed session takes the dossier last used where it works, so nobody has to bind it again. Returns the
/// line for the agent, and sets `active` when the binding worked.
fn rebind(
    store: &Dossiers,
    session: &str,
    dir: &Path,
    active: &mut Option<String>,
) -> Result<Option<String>> {
    let place = place_of(dir);
    let Some(last) = store.last_in(&place)? else {
        return Ok(None);
    };
    Ok(Some(
        match store.bind(session, "hook", &last, false, Some(&place)) {
            Ok(_) => {
                let line = format!(
                    "legionctl dossier: {last} (bound at session start: the last dossier used in {}; read its plan.md and \
                 newest MEMORY, and use another with dossier_use if this work needs one)",
                    place.display()
                );
                *active = Some(last);
                line
            }
            Err(e) => format!("legionctl: the last dossier here could not be bound: {e}"),
        },
    ))
}

/// Why this hook call stops the prompt (the gate, for the user) or the end of the reply (for the agent), if it does.
fn blocking_reason(
    resolved: &Resolved,
    input: &HookRunIn,
    has_session: bool,
    active: Option<&str>,
    place: Option<&Place>,
) -> Result<Option<String>> {
    let config = &resolved.config;
    // the gate: a session without a dossier gets no prompt through, except the one that binds a dossier
    let runs_dossier = input
        .prompt
        .as_deref()
        .is_some_and(|p| p.contains("legionctl:dossier"));
    if input.event == Event::Prompt
        && config.dossier.gate
        && has_session
        && active.is_none()
        && !runs_dossier
    {
        return gate_message(resolved).map(Some);
    }
    if input.event != Event::Stop {
        return Ok(None);
    }
    // before the reply ends, the agent (not the user) hears what is not set up, and sets it up first
    if config.dossier.nudge.contains(&Event::Stop) && has_session && active.is_none() {
        return Ok(Some(
            "legionctl: this session has no dossier. Before you end this reply, bind one with the legionctl MCP tool \
             dossier_use (a new name makes a new dossier; ask the user for the name if it is not clear), record the state \
             in its plan.md and MEMORY, then finish."
                .into(),
        ));
    }
    Ok(place
        .filter(|p| config.workspace.nudge && p.workspace.is_none() && p.main_dirty)
        .map(|p| {
            format!(
                "legionctl: the main checkout {} has changes but dossier {} has no workspace there. Open one with the \
                 legionctl MCP tool workspace_open and make code changes in its path; tell the user about the changes \
                 already in the main checkout, then finish.",
                p.repo.display(),
                active.unwrap_or_default()
            )
        }))
}

/// What a dossier nudge tells the agent: a line in its context, or a stop at the end of its reply.
enum Told {
    Line(String),
    Block(String),
}

/// The dossier nudge of a session that works in a workspace (`dossier.nudge_turns`): at the end of a reply, count it
/// when it changed the workspace; past the levels, tell the agent (info on the next prompt, warn once at the end of
/// a reply, block at the end of each reply). A dossier write answers it. Each new level and each answer is an event.
fn dossier_nudge(
    ctx: &Ctx,
    resolved: &Resolved,
    store: &Dossiers,
    (session, dossier, place): (&str, &str, &Place),
    event: Event,
) -> Result<Option<Told>> {
    let every = resolved.config.dossier.nudge_turns;
    let Some(ws) = place.workspace.as_ref().filter(|_| every > 0) else {
        return Ok(None);
    };
    let (turns, told) = match event {
        Event::Stop => store.turn(session, &legionctl_core::workspace::work_state(store, ws)?)?,
        Event::Prompt => store.nudge_state(session)?,
        Event::SessionStart => return Ok(None),
    };
    let level = match turns / every {
        0 => None,
        1 => Some("info"),
        2 => Some("warn"),
        _ => Some("block"),
    };
    if level != told.as_deref() {
        let db = Db::open_default()?;
        let payload = serde_json::json!({"session": session, "dossier": dossier, "level": level, "turns": turns});
        match level {
            None => db.record(&ctx.actor, "dossier.ack", &payload)?,
            Some(_) => db.record(&ctx.actor, "dossier.nudge", &payload)?,
        };
        store.told(session, level)?;
    }
    let text = format!(
        "legionctl: {turns} replies changed the workspace since this session last wrote dossier {dossier}. Record \
         the state in its plan.md and MEMORY now (MCP tools dossier_edit, dossier_add)."
    );
    Ok(match (event, level) {
        (Event::Prompt, Some(_)) => Some(Told::Line(text)),
        (Event::Stop, Some("warn")) if told.as_deref() != Some("warn") => Some(Told::Block(text)),
        (Event::Stop, Some("block")) => Some(Told::Block(format!(
            "{text} Until then, legionctl stops the end of each reply."
        ))),
        _ => None,
    })
}

/// What the agent reads first in a session: its dossier and its workspace, or what to set up.
fn start_lines(
    resolved: &Resolved,
    store: &Dossiers,
    session: &str,
    active: Option<&str>,
    rebound: Option<String>,
    place: Option<&Place>,
) -> Vec<String> {
    let nudge = resolved.config.dossier.nudge.contains(&Event::SessionStart);
    let mut lines = vec![match (active, rebound) {
        (Some(_), Some(line)) => line,
        (Some(d), None) => format!("legionctl dossier: {d} (session {session})"),
        (None, Some(failed)) if nudge => format!("{failed}\n{}", nudge_line(resolved)),
        (None, _) if nudge => nudge_line(resolved),
        (None, _) => "legionctl dossier: none".into(),
    }];
    match place.map(|p| (p, &p.workspace)) {
        Some((_, Some(w))) => {
            lines.push(format!(
                "legionctl workspace: {} on {} (base {}); make code changes there",
                w.path.display(),
                w.branch,
                w.base
            ));
            let drift = store.workspace_status(w).map(|s| s.drift).unwrap_or_default();
            lines.extend(drift.into_iter().map(|d| format!("legionctl workspace drift: {d}")));
        }
        Some((p, None)) if resolved.config.workspace.nudge => lines.push(format!(
            "legionctl: no workspace for {} yet. Before code changes, open one with the legionctl MCP tool \
             workspace_open and work in its path.",
            p.repo.display()
        )),
        _ => {}
    }
    lines
}

/// What the agent hears at session start when its session has no dossier.
fn nudge_line(resolved: &Resolved) -> String {
    format!(
        "legionctl: this session has no dossier yet. Set it up first, before other work: bind one with the legionctl \
         MCP tool dossier_use (a new name makes a new dossier; ask the user for the name if it is not clear).{}",
        if resolved.config.dossier.gate {
            " Until then, legionctl blocks the user's prompts except /legionctl:dossier."
        } else {
            ""
        }
    )
}

/// What a blocked prompt shows: why, and the commands that pass the gate, with the most recent dossiers.
fn gate_message(resolved: &Resolved) -> Result<String> {
    let recent: Vec<String> = Dossiers::open(&resolved.config.dossier)?
        .list()
        .into_iter()
        .take(5)
        .map(|d| d.name)
        .collect();
    Ok(format!(
        "legionctl: this session has no dossier, so the prompt was not sent. Run `/legionctl:dossier use <name>` \
         (a new name makes a new dossier), then send the prompt again.{}",
        if recent.is_empty() {
            String::new()
        } else {
            format!(" Recent dossiers: {}.", recent.join(", "))
        }
    ))
}

/// The config for work in `project` (its repository layer applies), or without a repository.
fn config(project: Option<&ClientPath>) -> Result<Resolved> {
    resolve(&Dirs::from_env(), path(project)?)
}

impl Operator {
    /// One telemetry sample over `window`; the root-only part comes from `legionctl-system`, and stays unknown when
    /// the daemon does not answer.
    pub fn sample(&self, window: std::time::Duration) -> Sample {
        let root_part = || {
            self.runtime
                .block_on(async {
                    let conn = zbus::Connection::system().await?;
                    let json = System1Proxy::new(&conn).await?.sample().await?;
                    anyhow::Ok(serde_json::from_str::<RootSample>(&json)?)
                })
                .unwrap_or_default()
        };
        telemetry::sample(&SysRoot::from_env(), window, &root_part)
    }
}

impl Engine for Operator {
    fn status(&self, _: &Ctx, input: StatusIn) -> Result<Snapshot> {
        let plan = Plan::load(path(input.profile.as_ref())?)?;
        let detail = if input.fast {
            Detail::Fast
        } else {
            Detail::Full
        };
        Ok(Snapshot::read(
            &plan.root,
            &plan.profile,
            detail,
            &config(None)?.config.load,
        ))
    }

    fn load(&self, _: &Ctx, _: Empty) -> Result<LoadState> {
        Ok(load::read_load(
            &SysRoot::from_env(),
            &config(None)?.config.load,
        ))
    }

    fn doctor(&self, _: &Ctx, input: ProfileIn) -> Result<Findings> {
        let plan = Plan::load(path(input.profile.as_ref())?)?;
        let mut findings = legionctl_machine::doctor::doctor(&plan.machine());
        match self.system(async |p| p.sample().await) {
            Ok(json) => {
                if let Some(e) = serde_json::from_str::<RootSample>(&json)?.runq_error {
                    findings.push(format!("the eBPF run-queue sensor does not run: {e}"));
                }
            }
            Err(e) => findings.push(format!(
                "legionctl-system does not answer (fans, throttle reasons and the eBPF sensor are unknown): {e:#}"
            )),
        }
        findings.extend(agent_findings()?);
        Ok(Findings { findings })
    }

    fn capabilities(&self, _: &Ctx, input: ProfileIn) -> Result<CapabilityList> {
        let (profile, _) = profile::load(path(input.profile.as_ref())?)?;
        Ok(CapabilityList {
            profile_id: profile.id,
            capabilities: profile.capabilities,
        })
    }

    fn validate(&self, _: &Ctx, input: ProfileIn) -> Result<Validation> {
        let path =
            path(input.profile.as_ref())?.map_or_else(profile::default_path, Path::to_path_buf);
        Ok(Validation {
            problems: profile::validate(&path)?,
            path: path.display().to_string(),
        })
    }

    fn gpu_status(&self, _: &Ctx, _: Empty) -> Result<GpuState> {
        let root = SysRoot::from_env();
        let outputs = gpu::outputs(&root);
        Ok(GpuState {
            mode: gpu::mode(&outputs).into(),
            desktop_gpu: gpu::desktop_preference(&root).into(),
            outputs,
        })
    }

    fn gpu_run_env(&self, _: &Ctx, input: RunEnvIn) -> Result<RunEnv> {
        let root = SysRoot::from_env();
        let gpu = serde_json::to_value(input.gpu)?;
        let cores = serde_json::to_value(input.cores)?;
        let (gpu, cores) = (
            gpu.as_str().unwrap_or("default"),
            cores.as_str().unwrap_or("all"),
        );
        let cpus = match cores {
            "all" => None,
            class => Some(
                gpu::core_class_cpus(&root, class)
                    .context("this CPU does not report performance/efficiency core groups")?,
            ),
        };
        Ok(RunEnv {
            env: gpu::render_env(&root, gpu)
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            cpus,
        })
    }

    fn power_apply(&self, _: &Ctx, input: PowerApplyIn) -> Result<Changes> {
        let source = match input.source {
            PowerSource::Auto => None,
            PowerSource::Ac => Some(Source::Ac),
            PowerSource::Battery => Some(Source::Battery),
        };
        if input.dry_run {
            let plan = Plan::load(None)?;
            return Ok(dry_run(
                &plan.machine().apply(source, Options::default())?.1,
            ));
        }
        let name = serde_json::to_value(input.source)?;
        let name = name.as_str().unwrap_or("auto").to_string();
        Ok(Changes {
            lines: self.system(async move |p| p.apply(&name).await)?,
            dry_run: false,
        })
    }

    fn gpu_primary(&self, _: &Ctx, input: GpuPrimaryIn) -> Result<Changes> {
        let choice = match input.choice {
            DesktopGpu::Nvidia => "nvidia",
            DesktopGpu::Intel => "intel",
            DesktopGpu::Auto => "auto",
        };
        if input.dry_run {
            let plan = Plan::load(None)?;
            return Ok(dry_run(&gpu::set_desktop(&plan.machine(), choice)?));
        }
        Ok(Changes {
            lines: self.system(async move |p| p.set_desktop_gpu(choice).await)?,
            dry_run: false,
        })
    }

    fn cleanup(&self, _: &Ctx, input: CleanupIn) -> Result<Changes> {
        if input.dry_run {
            let plan = Plan::load(None)?;
            let opts = cleanup::Options {
                force: false,
                unload_modules: input.unload_modules,
            };
            return Ok(dry_run(&cleanup::plan(&plan.machine(), opts)?));
        }
        Ok(Changes {
            lines: self.system(async move |p| p.cleanup(input.unload_modules).await)?,
            dry_run: false,
        })
    }

    fn agents_list(&self, _: &Ctx, input: ProjectIn) -> Result<ConfigFiles> {
        Ok(agents::config_files(&home(), input.project.get()?))
    }

    fn agents_set(&self, _: &Ctx, input: AgentSetIn) -> Result<Edit> {
        let project = input.project.get()?;
        let path = match (input.agent, input.scope) {
            (AgentName::Claude, SettingsScope::User) => home().join(".claude/settings.json"),
            (AgentName::Claude, SettingsScope::Project) => project.join(".claude/settings.json"),
            (AgentName::Claude, SettingsScope::ProjectLocal) => {
                project.join(".claude/settings.local.json")
            }
            (AgentName::Codex, SettingsScope::User) => home().join(".codex/config.toml"),
            (AgentName::Codex, SettingsScope::Project) => project.join(".codex/config.toml"),
            (AgentName::Codex, SettingsScope::ProjectLocal) => {
                anyhow::bail!("Codex has no project-local settings; use --scope user")
            }
        };
        agents::set(
            &path,
            &input.key,
            agents::value_of(&input.value),
            input.dry_run,
        )
    }

    fn agents_usage(&self, _: &Ctx, input: AgentUsageIn) -> Result<UsageReport> {
        let (claude, codex) = match &input.account {
            Some(account) => {
                let profile = legionctl_core::session::account_dir(account)?;
                (profile.join("claude"), profile.join("codex"))
            }
            None => (home().join(".claude"), home().join(".codex")),
        };
        Ok(agents::usage(&claude, &codex, input.days))
    }

    fn agents_plugins(&self, _: &Ctx, _: Empty) -> Result<Vec<Plugin>> {
        let plugins = config(None)?.config.plugins;
        let out = Command::new("claude")
            .args(["plugin", "list", "--json"])
            .output()
            .context("running claude plugin list")?;
        anyhow::ensure!(
            out.status.success(),
            "claude plugin list: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        let list: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout)?;
        Ok(list
            .iter()
            .map(|p| {
                let id = p["id"].as_str().unwrap_or_default().to_string();
                Plugin {
                    allowed: plugins.refusal(&id).is_none(),
                    version: p["version"].as_str().unwrap_or_default().into(),
                    scope: p["scope"].as_str().unwrap_or_default().into(),
                    enabled: p["enabled"] == true,
                    id,
                }
            })
            .collect())
    }

    fn agents_plugin_install(&self, _: &Ctx, input: PluginInstallIn) -> Result<Lines> {
        // the user's and the system's layers only: a repository must not widen the allowlist it is checked against
        let plugins = config(None)?.config.plugins;
        if let Some(why) = plugins.refusal(&input.id) {
            anyhow::bail!("{why}");
        }
        let (plugin, market) = input.id.split_once('@').context("checked above")?;
        let scope = match input.scope {
            SettingsScope::User => "user",
            SettingsScope::Project => "project",
            SettingsScope::ProjectLocal => "local",
        };
        // never -y: a command the marketplace declares is for a person to accept
        let out = Command::new("claude")
            .current_dir(input.project.get()?)
            .args(["plugin", "install", plugin, "--marketplace"])
            .arg(&plugins.marketplaces[market].source)
            .args(["--scope", scope, "--json"])
            .output()
            .context("running claude plugin install")?;
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
        anyhow::ensure!(out.status.success(), "claude plugin install: {said}");
        Ok(Lines {
            lines: vec![format!("installed {} ({scope})", input.id), said],
        })
    }

    fn agents_accounts(&self, _: &Ctx, _: Empty) -> Result<Vec<Account>> {
        let root = legionctl_core::session::accounts_root();
        let mut out: Vec<Account> = std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| {
                let dir = e.path();
                Account {
                    name: e.file_name().to_string_lossy().into_owned(),
                    // the files each agent writes at login
                    claude: dir.join("claude/.credentials.json").is_file(),
                    codex: dir.join("codex/auth.json").is_file(),
                    dir: dir.display().to_string(),
                }
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn agents_account_add(&self, _: &Ctx, input: IdIn) -> Result<Lines> {
        anyhow::ensure!(
            legionctl_core::dossier::valid_name(&input.id),
            "an account name is a-z, 0-9 and '-', starting with a letter or digit"
        );
        let dir = legionctl_core::session::accounts_root().join(&input.id);
        for agent in ["claude", "codex"] {
            std::fs::create_dir_all(dir.join(agent))?;
        }
        Ok(Lines {
            lines: vec![
                format!("account profile {}", dir.display()),
                format!(
                    "log in: CLAUDE_CONFIG_DIR={}/claude claude, then /login",
                    dir.display()
                ),
                format!("        CODEX_HOME={}/codex codex login", dir.display()),
                format!(
                    "use it: legionctl session start claude --account {}",
                    input.id
                ),
            ],
        })
    }

    fn workspace_open(&self, _: &Ctx, input: WorkspaceOpenIn) -> Result<Workspace> {
        let root = config(None)?.config.workspace.root;
        dossiers()?.open_workspace(
            session(&input.session)?,
            input.repo.get()?,
            input.base.as_deref(),
            &root,
        )
    }

    fn workspace_status(&self, _: &Ctx, input: WorkspaceIn) -> Result<WorkspaceStates> {
        let store = dossiers()?;
        let id = session(&input.session)?;
        let open = if let Some(repo) = path(input.repo.as_ref())? {
            vec![store.workspace_of(id, Some(repo))?]
        } else {
            let dossier = store
                .active(id)?
                .context("this session has no active dossier")?;
            store.workspaces(Some(&dossier))?
        };
        open.iter().map(|ws| store.workspace_status(ws)).collect()
    }

    fn workspace_list(&self, _: &Ctx, _: Empty) -> Result<Workspaces> {
        dossiers()?.workspaces(None)
    }

    fn workspace_checkpoint(&self, _: &Ctx, input: CheckpointIn) -> Result<Lines> {
        let store = dossiers()?;
        let ws = store.workspace_of(session(&input.session)?, path(input.repo.as_ref())?)?;
        Ok(Lines {
            lines: vec![store.checkpoint(&ws, &input.message)?],
        })
    }

    fn workspace_branch(&self, _: &Ctx, input: BranchIn) -> Result<Lines> {
        let store = dossiers()?;
        let ws = store.workspace_of(session(&input.session)?, path(input.repo.as_ref())?)?;
        Ok(Lines {
            lines: vec![format!(
                "on {}",
                store.switch_branch(&ws, input.topic.as_deref())?
            )],
        })
    }

    fn workspace_merge(&self, _: &Ctx, input: WorkspaceIn) -> Result<Lines> {
        let store = dossiers()?;
        let ws = store.workspace_of(session(&input.session)?, path(input.repo.as_ref())?)?;
        Ok(Lines {
            lines: store.merge(&ws)?,
        })
    }

    fn workspace_close(&self, _: &Ctx, input: CloseIn) -> Result<Lines> {
        let store = dossiers()?;
        let ws = store.workspace_of(session(&input.session)?, path(input.repo.as_ref())?)?;
        Ok(Lines {
            lines: vec![store.close(&ws, input.force)?],
        })
    }

    fn setup_check(&self, _: &Ctx, _: Empty) -> Result<SetupReport> {
        Ok(setup::check())
    }

    fn setup_apply(&self, _: &Ctx, input: SetupApplyIn) -> Result<Changes> {
        Ok(Changes {
            lines: setup::apply(input.install_ctx)?,
            dry_run: false,
        })
    }

    fn reminders_list(&self, _: &Ctx, input: ProjectIn) -> Result<Reminders> {
        Ok(reminders::list(&config(Some(&input.project))?))
    }

    fn reminders_on(&self, _: &Ctx, input: IdIn) -> Result<Reminders> {
        reminders::set_enabled(&config(Some(&input.project))?, &input.id, true)?;
        Ok(reminders::list(&config(Some(&input.project))?))
    }

    fn reminders_off(&self, _: &Ctx, input: IdIn) -> Result<Reminders> {
        reminders::set_enabled(&config(Some(&input.project))?, &input.id, false)?;
        Ok(reminders::list(&config(Some(&input.project))?))
    }

    fn reminders_add(&self, _: &Ctx, input: ReminderAddIn) -> Result<Reminders> {
        reminders::add(
            &config(Some(&input.project))?,
            &input.id,
            &input.text,
            input.event,
        )?;
        Ok(reminders::list(&config(Some(&input.project))?))
    }

    fn reminders_rm(&self, _: &Ctx, input: IdIn) -> Result<Reminders> {
        reminders::remove(&config(Some(&input.project))?, &input.id)?;
        Ok(reminders::list(&config(Some(&input.project))?))
    }

    fn hooks_list(&self, _: &Ctx, input: ProjectIn) -> Result<HookTable> {
        Ok(Self::hook_table(&config(Some(&input.project))?))
    }

    fn hooks_bind(&self, _: &Ctx, input: HookBindIn) -> Result<HookTable> {
        hooks::bind(&config(Some(&input.project))?, input.event, &input.action)?;
        Ok(Self::hook_table(&config(Some(&input.project))?))
    }

    fn hooks_unbind(&self, _: &Ctx, input: HookBindIn) -> Result<HookTable> {
        hooks::unbind(&config(Some(&input.project))?, input.event, &input.action)?;
        Ok(Self::hook_table(&config(Some(&input.project))?))
    }

    fn hooks_add(&self, _: &Ctx, input: HookAddIn) -> Result<HookTable> {
        hooks::add_command(
            &config(Some(&input.project))?,
            &input.id,
            &input.command,
            input.timeout,
        )?;
        Ok(Self::hook_table(&config(Some(&input.project))?))
    }

    fn hooks_rm(&self, _: &Ctx, input: IdIn) -> Result<HookTable> {
        hooks::remove_action(&config(Some(&input.project))?, &input.id)?;
        Ok(Self::hook_table(&config(Some(&input.project))?))
    }

    fn hooks_run(&self, ctx: &Ctx, input: HookRunIn) -> Result<HookReply> {
        let resolved = config(Some(&input.project))?;
        let store = Dossiers::open(&resolved.config.dossier)?;
        let session = input.session.as_ref().filter(|s| !s.0.is_empty());
        let mut active = match session {
            Some(s) => store.active(&s.0)?,
            None => None,
        };
        if let (Some(s), Some(_)) = (session, &active) {
            // each hook call shows the session is live, so its dossier is not taken over
            store.touch(&s.0)?;
        }
        let rebound = match session {
            Some(s)
                if input.event == Event::SessionStart
                    && active.is_none()
                    && resolved.config.dossier.auto_bind =>
            {
                rebind(&store, &s.0, input.project.get()?, &mut active)?
            }
            _ => None,
        };
        let place = active
            .as_deref()
            .zip(input.project.get().ok())
            .and_then(|(d, dir)| store.place(d, dir));
        if let Some(block) = blocking_reason(
            &resolved,
            &input,
            session.is_some(),
            active.as_deref(),
            place.as_ref(),
        )? {
            return Ok(HookReply {
                text: String::new(),
                block: Some(block),
            });
        }
        let mut lines = Vec::new();
        if let (Some(s), Some(d), Some(p)) = (session, active.as_deref(), place.as_ref())
            && let Some(told) = dossier_nudge(ctx, &resolved, &store, (&s.0, d, p), input.event)?
        {
            match told {
                Told::Line(line) => lines.push(line),
                Told::Block(reason) => {
                    return Ok(HookReply {
                        text: String::new(),
                        block: Some(reason),
                    });
                }
            }
        }
        if input.event == Event::SessionStart
            && let Some(s) = session
        {
            lines.extend(start_lines(
                &resolved,
                &store,
                &s.0,
                active.as_deref(),
                rebound,
                place.as_ref(),
            ));
        }
        let ran = hooks::run(&resolved, input.event);
        if !ran.is_empty() {
            lines.push(ran);
        }
        Ok(HookReply {
            text: lines.join("\n"),
            block: None,
        })
    }

    fn dossier_use(&self, ctx: &Ctx, input: DossierUseIn) -> Result<Lines> {
        let lines = dossiers()?.bind(
            session(&input.session)?,
            &ctx.actor,
            &input.name,
            input.steal,
            Some(&place_of(input.project.get()?)),
        )?;
        Ok(Lines { lines })
    }

    fn dossier_active(&self, _: &Ctx, input: SessionIn) -> Result<Lines> {
        let id = session(&input.session)?;
        let active = dossiers()?.active(id)?;
        Ok(Lines {
            lines: vec![format!(
                "session {id}: active dossier {}",
                active.as_deref().unwrap_or("(none)")
            )],
        })
    }

    fn dossier_release(&self, _: &Ctx, input: SessionIn) -> Result<Lines> {
        let released = dossiers()?.release(session(&input.session)?)?;
        Ok(Lines {
            lines: vec![
                released.map_or_else(|| "no active dossier".into(), |d| format!("released {d}")),
            ],
        })
    }

    fn dossier_locks(&self, _: &Ctx, _: Empty) -> Result<Locks> {
        dossiers()?.locks()
    }

    fn dossier_ls(&self, _: &Ctx, _: Empty) -> Result<DossierList> {
        Ok(dossiers()?.list())
    }

    fn dossier_read(&self, _: &Ctx, input: DossierReadIn) -> Result<Lines> {
        let lines = dossiers()?.read(
            &input.path,
            input.first as usize,
            input.last.map(|l| l as usize),
        )?;
        Ok(Lines { lines })
    }

    fn dossier_search(&self, _: &Ctx, input: DossierSearchIn) -> Result<Lines> {
        let lines = dossiers()?.search(
            &input.pattern,
            input.dossier.as_deref(),
            input.ignore_case,
            input.limit as usize,
        );
        Ok(Lines { lines })
    }

    fn dossier_write(&self, _: &Ctx, input: DossierWriteIn) -> Result<Lines> {
        let path = dossiers()?.write(
            session(&input.session)?,
            &input.path,
            &input.content.0,
            input.overwrite,
        )?;
        Ok(Lines {
            lines: vec![path.display().to_string()],
        })
    }

    fn dossier_append(&self, _: &Ctx, input: DossierAppendIn) -> Result<Lines> {
        let path = dossiers()?.append(session(&input.session)?, &input.path, &input.content.0)?;
        Ok(Lines {
            lines: vec![path.display().to_string()],
        })
    }

    fn dossier_edit(&self, _: &Ctx, input: DossierEditIn) -> Result<Lines> {
        let lines = dossiers()?.edit(
            session(&input.session)?,
            &input.path,
            input.mode,
            &input.start,
            input.end.as_deref(),
            &input.content.0,
        )?;
        Ok(Lines { lines })
    }

    fn dossier_add(&self, _: &Ctx, input: DossierAddIn) -> Result<Lines> {
        let rel = dossiers()?.add(
            session(&input.session)?,
            &input.class,
            &input.slug,
            &input.content.0,
        )?;
        Ok(Lines { lines: vec![rel] })
    }

    fn dossier_nudges(&self, _: &Ctx, input: DossierNudgesIn) -> Result<Events> {
        let db = Db::open_default()?;
        let mut rows = db.events_of("dossier.nudge")?;
        rows.extend(db.events_of("dossier.ack")?);
        rows.retain(|e| {
            input
                .of
                .as_deref()
                .is_none_or(|s| e.payload["session"] == s)
        });
        rows.sort_by_key(|e| e.id);
        Ok(rows)
    }

    fn dossier_graph(&self, _: &Ctx, input: DossierGraphIn) -> Result<Graph> {
        let ws = dossiers()?;
        let dossier = match input.dossier {
            Some(d) => d,
            None => ws
                .active(session(&input.session)?)?
                .context("this session has no active dossier; add --dossier <name>")?,
        };
        ws.graph(&dossier, input.node.as_deref())
    }

    fn dossier_check(&self, _: &Ctx, input: DossierCheckIn) -> Result<DossierFindings> {
        let ws = dossiers()?;
        let dossier = match input.dossier {
            Some(d) => d,
            None => ws
                .active(session(&input.session)?)?
                .context("this session has no active dossier; add --dossier <name>")?,
        };
        Ok(DossierFindings {
            problems: ws.check(&dossier),
            dossier,
        })
    }

    fn dossier_guard(&self, _: &Ctx, input: GuardIn) -> Result<Guard> {
        Ok(Guard {
            refusal: dossiers()?.write_refusal(session(&input.session)?, Path::new(&input.path))?,
        })
    }

    fn skills(&self, _: &Ctx, input: ProjectIn) -> Result<SkillList> {
        Ok(skills::inventory(&home(), &config(Some(&input.project))?))
    }

    fn monitor(&self, _: &Ctx, input: MonitorIn) -> Result<Samples> {
        anyhow::ensure!((1..=3600).contains(&input.count), "count must be 1-3600");
        anyhow::ensure!(
            (100..=60_000).contains(&input.interval_ms),
            "--interval-ms must be 100-60000"
        );
        let window = std::time::Duration::from_millis(u64::from(input.interval_ms));
        Ok((0..input.count).map(|_| self.sample(window)).collect())
    }

    fn events(&self, _: &Ctx, input: EventsIn) -> Result<Events> {
        Db::open_default()?.events(input.since, input.limit)
    }

    fn packages_list(&self, _: &Ctx, _: Empty) -> Result<Packages> {
        Store::new(Store::default_root()).list(&config(None)?.config.packages)
    }

    fn packages_fetch(&self, _: &Ctx, input: PackageFetchIn) -> Result<Manifest> {
        let resolved = config(None)?;
        let package = resolved
            .config
            .packages
            .get(&input.name)
            .with_context(|| format!("no package {} in the config (packages)", input.name))?;
        Store::new(Store::default_root()).fetch(
            &input.name,
            package,
            input.force,
            &resolved.config.session.hidden,
        )
    }

    fn gateway_usage(&self, _: &Ctx, input: GatewayUsageIn) -> Result<Vec<GatewayUsage>> {
        Db::open_default()?.gateway_usage(input.since)
    }

    fn cache_get(&self, _: &Ctx, input: CacheGetIn) -> Result<CacheValue> {
        Ok(CacheValue {
            value: Db::open_default()?.cache_get(&input.key)?,
        })
    }

    fn cache_put(&self, _: &Ctx, input: CachePutIn) -> Result<Empty> {
        Db::open_default()?.cache_put(&input.key, &input.value, input.ttl_s)?;
        Ok(Empty {})
    }

    // the operator's audit row of this call is the record
    fn tools_record(&self, _: &Ctx, _: ToolCall) -> Result<Empty> {
        Ok(Empty {})
    }

    fn tools_calls(&self, _: &Ctx, input: ToolCallsIn) -> Result<ToolCalls> {
        Db::open_default()?
            .events_of("internal.tools.record")?
            .into_iter()
            .map(|e| {
                Ok(ToolCallRow {
                    id: e.id,
                    ts: e.ts,
                    call: serde_json::from_value(e.payload["input"].clone())?,
                })
            })
            .filter(|row: &Result<ToolCallRow>| {
                row.as_ref().map_or(true, |r| {
                    input
                        .session
                        .as_ref()
                        .is_none_or(|s| *s == r.call.session.0)
                })
            })
            .collect()
    }

    fn images_scan(&self, _: &Ctx, input: ImagesScanIn) -> Result<Scan> {
        let repo = input.repo.get()?;
        let scan = legionctl_core::images::scan_repository(repo)?;
        if input.write {
            let dir = repo.join(".legionctl");
            std::fs::create_dir_all(&dir)?;
            std::fs::write(
                dir.join("docker-compose-services.json"),
                serde_json::to_string_pretty(&scan.services)? + "\n",
            )?;
            std::fs::write(
                dir.join("docker-compose-cache.json"),
                serde_json::to_string_pretty(&scan.cache)? + "\n",
            )?;
        }
        Ok(scan)
    }

    fn images_build(&self, _: &Ctx, input: RepoIn) -> Result<Lines> {
        let repo = input.repo.get()?;
        let (project, tag) = legionctl_core::images::project(repo);
        let mut lines = Vec::new();
        for (name, service) in legionctl_core::images::scan_repository(repo)?.services {
            // a prebuilt image from the compose file: compose pulls it
            let Some(spec) = service.build else {
                continue;
            };
            let image = legionctl_core::images::image(&project, &name, &tag);
            let mut build = Command::new("docker");
            build.current_dir(repo).args([
                "buildx",
                "build",
                "--load",
                "-t",
                &image,
                "-f",
                &spec.dockerfile,
            ]);
            if let Some(target) = &spec.target {
                build.args(["--target", target]);
            }
            let out = build
                .arg(&spec.context)
                .output()
                .context("running docker buildx")?;
            anyhow::ensure!(
                out.status.success(),
                "building {name}: {}",
                String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .rev()
                    .take(5)
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
            lines.push(format!("built {image}"));
        }
        Ok(Lines { lines })
    }

    fn images_run(&self, _: &Ctx, input: RepoIn) -> Result<Lines> {
        let repo = input.repo.get()?;
        let (project, tag) = legionctl_core::images::project(repo);
        let scan = legionctl_core::images::scan_repository(repo)?;
        let compose = legionctl_core::images::compose(&scan, &project, &tag);
        let file = repo.join(".legionctl/compose.json");
        std::fs::create_dir_all(file.parent().context("no parent")?)?;
        std::fs::write(&file, serde_json::to_string_pretty(&compose)? + "\n")?;
        let mut up = Command::new("docker");
        up.current_dir(repo).args(["compose", "-p", &project]);
        // the repository's own file first: legionctl's file only adds the built tags
        if let Some(own) = &scan.compose_file {
            up.args(["-f", own]);
        }
        let out = up
            .arg("-f")
            .arg(&file)
            .args(["up", "-d", "--no-build"])
            .output()
            .context("running docker compose")?;
        anyhow::ensure!(
            out.status.success(),
            "docker compose up: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        Ok(Lines {
            lines: vec![format!("started project {project} from {}", file.display())],
        })
    }

    fn images_status(&self, _: &Ctx, input: RepoIn) -> Result<Vec<ServiceState>> {
        let (project, _) = legionctl_core::images::project(input.repo.get()?);
        self.runtime
            .block_on(crate::virt::project_containers(&project))
    }

    fn virt_act(&self, _: &Ctx, input: VirtActIn) -> Result<Lines> {
        let action = serde_json::to_value(input.action)?;
        let line = self.runtime.block_on(crate::virt::act(
            action.as_str().unwrap_or_default(),
            &input.name,
        ))?;
        Ok(Lines { lines: vec![line] })
    }

    fn virt_status(&self, _: &Ctx, _: Empty) -> Result<VirtStatus> {
        let low = config(None)?.config.load.busy.memory_available_percent;
        Ok(self
            .runtime
            .block_on(crate::virt::status(&SysRoot::from_env(), low)))
    }

    fn virt_tune(&self, _: &Ctx, input: VirtTuneIn) -> Result<Lines> {
        let cpus = core_cpus(input.cores)?;
        let memory = input.memory.as_deref().map(parse_bytes).transpose()?;
        anyhow::ensure!(
            cpus.is_some() || memory.is_some(),
            "nothing to tune: give --cores or --memory"
        );
        let mut lines = self.runtime.block_on(crate::virt::tune(
            &input.name,
            cpus.as_deref(),
            memory,
            !input.dry_run,
        ))?;
        lines.push(
            if input.dry_run {
                "dry run: nothing changed"
            } else {
                "applied"
            }
            .into(),
        );
        Ok(Lines { lines })
    }

    fn virt_create(&self, _: &Ctx, input: VirtCreateIn) -> Result<Lines> {
        let spec = crate::virt::ContainerSpec {
            name: input.name,
            image: input.image,
            cpus: core_cpus(input.cores)?,
            memory: input.memory.as_deref().map(parse_bytes).transpose()?,
            gpu: input.gpu,
        };
        let mut lines = self
            .runtime
            .block_on(crate::virt::create(&spec, !input.dry_run))?;
        lines.push(
            if input.dry_run {
                "dry run: nothing created"
            } else {
                "created; start it with legionctl virt act --action start <name>"
            }
            .into(),
        );
        Ok(Lines { lines })
    }

    fn windows_status(&self, _: &Ctx, _: Empty) -> Result<WindowsState> {
        windows::state()
    }

    fn windows_reboot(&self, _: &Ctx, input: DryRunIn) -> Result<Changes> {
        if input.dry_run {
            return Ok(dry_run(&windows::reboot_plan(&windows::state()?)?));
        }
        Ok(Changes {
            lines: self.system(async |p| p.reboot_to_windows().await)?,
            dry_run: false,
        })
    }

    fn record(&self, _: &Ctx, input: RecordIn) -> Result<Recording> {
        let dir = input.dir.get()?;
        anyhow::ensure!(
            std::fs::read_dir(dir).map_or(true, |mut d| d.next().is_none()),
            "{} is not empty; record into a new directory",
            dir.display()
        );
        let root = SysRoot::from_env().traced();
        let plan = Plan::at(root.clone(), None)?;
        let since = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
        )? - i64::from(input.minutes) * 60;
        // only the machine's events: agent payloads (prompts, dossier text) stay out of a recording that is shared
        let events = Db::open_default()?
            .events_from(since)?
            .into_iter()
            .filter(|e| e.actor == "legionctl-system" || MACHINE_OPS.contains(&e.kind.as_str()))
            .collect();
        // status reads more files than the decisions do; recording them lets every read operation replay
        Snapshot::read(
            &plan.root,
            &plan.profile,
            Detail::Full,
            &config(None)?.config.load,
        );
        let mut recording = plan.decisions(dir, events)?;
        let tree = dir.join("root");
        recording.paths = root.record_into(&tree)?;
        for file in [profile::DEFAULT_PATH, legionctl_machine::ledger::DB] {
            if let Ok(bytes) = std::fs::read(root.path(file)) {
                let to = SysRoot::new(&tree).path(file);
                std::fs::create_dir_all(to.parent().context("no parent")?)?;
                std::fs::write(to, bytes)?;
            }
        }
        std::fs::write(
            dir.join("events.json"),
            serde_json::to_string_pretty(&recording.events)?,
        )?;
        Ok(recording)
    }

    fn replay(&self, _: &Ctx, input: ReplayIn) -> Result<Recording> {
        let dir = input.dir.get()?;
        let events = std::fs::read(dir.join("events.json"))
            .with_context(|| format!("{} is not a recording", dir.display()))?;
        Plan::at(SysRoot::new(dir.join("root")), None)?
            .decisions(dir, serde_json::from_slice(&events)?)
    }

    fn bench_run(&self, ctx: &Ctx, input: BenchIn) -> Result<BenchRun> {
        anyhow::ensure!((1..=600).contains(&input.seconds), "seconds must be 1-600");
        let knobs = knobs::catalog(&Plan::load(None)?.machine())
            .into_iter()
            .map(|k| (k.id, k.value))
            .collect();
        let runs = input
            .scenario
            .commands(input.seconds)
            .into_iter()
            .map(|argv| {
                let child = std::process::Command::new(&argv[0])
                    .args(&argv[1..])
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .with_context(|| format!("starting {} (is it installed?)", argv[0]))?;
                // a thread drains the pipes, so a chatty workload never blocks on a full pipe
                Ok((
                    argv[0].clone(),
                    std::thread::spawn(move || child.wait_with_output()),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut samples = Vec::new();
        while runs.iter().any(|(_, r)| !r.is_finished()) {
            samples.push(self.sample(std::time::Duration::from_secs(1)));
        }
        let mut scores = bench::Scores::default();
        for (program, run) in runs {
            let out = run
                .join()
                .map_err(|_| anyhow::anyhow!("{program} panicked"))??;
            let text = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
            if program == "gpu-burn" {
                bench::gpu_burn(&text, &mut scores);
            } else {
                bench::stress_ng(&text, &mut scores);
            }
            if !out.status.success() {
                scores
                    .problems
                    .push(format!("{program} exited with {}", out.status));
            }
        }
        let result = BenchResult {
            scenario: input.scenario,
            label: input.label,
            seconds: input.seconds,
            scores: scores.scores,
            problems: scores.problems,
            summary: bench::summarize(&samples),
            knobs,
        };
        let db = Db::open_default()?;
        let id = db.record(&ctx.actor, "bench.result", &serde_json::to_value(&result)?)?;
        bench_run_of(
            db.events_of("bench.result")?
                .into_iter()
                .find(|e| e.id == id)
                .context("the run was not stored")?,
        )
    }

    fn tune(&self, _: &Ctx, input: TuneIn) -> Result<TuneReport> {
        let load = load::read_load(&SysRoot::from_env(), &config(None)?.config.load);
        anyhow::ensure!(
            input.force || load.level == load::Level::Idle,
            "the machine is not idle ({}); stop other work, or add --force",
            load.reasons.join("; ")
        );
        // the daemon has its own PATH: give it the programs' full paths
        let workloads = input
            .scenario
            .commands(input.seconds)
            .into_iter()
            .map(|mut argv| {
                argv[0] = which(&argv[0])
                    .with_context(|| format!("{} is not installed", argv[0]))?
                    .display()
                    .to_string();
                Ok(argv)
            })
            .collect::<Result<_>>()?;
        let spec = serde_json::to_string(&legionctl_machine::tune::TuneSpec {
            attribute: input.attribute,
            values: input.values,
            scenario: input.scenario,
            seconds: input.seconds,
            cooldown_s: input.cooldown_s,
            workloads,
        })?;
        let report = self.system(async move |p| p.tune(&spec).await)?;
        Ok(serde_json::from_str(&report)?)
    }

    fn bench_list(&self, _: &Ctx, _: Empty) -> Result<BenchRuns> {
        Db::open_default()?
            .events_of("bench.result")?
            .into_iter()
            .map(bench_run_of)
            .collect()
    }

    fn bench_compare(&self, _: &Ctx, input: BenchCompareIn) -> Result<BenchCompare> {
        let runs = Db::open_default()?.events_of("bench.result")?;
        let run = |id: i64| {
            runs.iter()
                .find(|e| e.id == id)
                .cloned()
                .with_context(|| format!("no bench run {id} (see legionctl bench list)"))
                .and_then(bench_run_of)
        };
        Ok(BenchCompare {
            a: run(input.a)?,
            b: run(input.b)?,
        })
    }

    fn knobs(&self, _: &Ctx, input: ProfileIn) -> Result<Knobs> {
        Ok(knobs::catalog(
            &Plan::load(path(input.profile.as_ref())?)?.machine(),
        ))
    }

    fn inspect(&self, _: &Ctx, input: InspectIn) -> Result<KnobDetail> {
        let plan = Plan::load(path(input.profile.as_ref())?)?;
        let knob = knobs::find(&plan.machine(), &input.knob).with_context(|| {
            format!(
                "no knob {} on this machine (see legionctl knobs)",
                input.knob
            )
        })?;
        let history = Db::open_default()?.events_naming(&knob.path, input.limit)?;
        Ok(KnobDetail { knob, history })
    }

    fn session_start(&self, _: &Ctx, input: SessionStartIn) -> Result<Launch> {
        let dir = input.repo.get()?;
        let resolved = config(Some(&input.repo))?;
        let store = Dossiers::open(&resolved.config.dossier)?;
        let dossier = match input.dossier {
            Some(d) => d,
            None => store.last_in(&place_of(dir))?.context(
                "no dossier was used in this repository yet; add --dossier <name> --create",
            )?,
        };
        let token = legionctl_api::random_token()?;
        let start = Start {
            agent: &input.agent,
            dossier: &dossier,
            steal: input.steal,
            token: &token,
            dir,
            headless: false,
            account: input.account.as_deref(),
        };
        let launch = store.start_session(&start, &resolved.config)?;
        Db::open_default()?.issue_token(&launch.id, &token)?;
        Ok(launch)
    }

    fn session_run(&self, _: &Ctx, input: SessionRunIn) -> Result<RunOutput> {
        let dir = input.repo.get()?;
        let resolved = config(Some(&input.repo))?;
        let store = Dossiers::open(&resolved.config.dossier)?;
        let dossier = match input.dossier {
            Some(d) => d,
            None => store.last_in(&place_of(dir))?.context(
                "no dossier was used in this repository yet; add --dossier <name> --create",
            )?,
        };
        let token = legionctl_api::random_token()?;
        let start = Start {
            agent: &input.agent,
            dossier: &dossier,
            steal: input.steal,
            token: &token,
            dir,
            headless: true,
            account: input.account.as_deref(),
        };
        let launch = store.start_session(&start, &resolved.config)?;
        Db::open_default()?.issue_token(&launch.id, &token)?;
        self.runtime
            .block_on(crate::acp::run(launch, &input.prompt))
    }

    fn session_list(&self, _: &Ctx, _: Empty) -> Result<Sessions> {
        Ok(dossiers()?
            .sessions()?
            .into_iter()
            .map(|session| {
                let state = legionctl_core::session::scope_state(&session.id);
                SessionState { state, session }
            })
            .collect())
    }

    fn session_stop(&self, _: &Ctx, input: SessionIdIn) -> Result<Lines> {
        scope_control("stop", &input.id, "stopped")
    }

    fn session_activity(&self, _: &Ctx, input: SessionIdIn) -> Result<Vec<SessionExec>> {
        Ok(Db::open_default()?
            .events_of("session.exec")?
            .into_iter()
            .filter(|e| e.payload["session"] == input.id.as_str())
            .filter_map(|e| {
                Some(SessionExec {
                    ts: e.ts,
                    pid: u32::try_from(e.payload["pid"].as_u64()?).ok()?,
                    argv: serde_json::from_value(e.payload["argv"].clone()).ok()?,
                })
            })
            .collect())
    }

    fn session_pause(&self, _: &Ctx, input: SessionIdIn) -> Result<Lines> {
        scope_control("freeze", &input.id, "paused")
    }

    fn session_thaw(&self, _: &Ctx, input: SessionIdIn) -> Result<Lines> {
        scope_control("thaw", &input.id, "running again")
    }

    fn session_resume(&self, _: &Ctx, input: SessionResumeIn) -> Result<Launch> {
        // the repository's own config layer applies, as at session start
        let session = dossiers()?.session(&input.id)?;
        let resolved = resolve(&Dirs::from_env(), Some(&place_of(&session.workdir)))?;
        let token = legionctl_api::random_token()?;
        let launch = dossiers()?.resume_session(
            &input.id,
            &resolved.config,
            input.account.as_deref(),
            &token,
        )?;
        Db::open_default()?.issue_token(&launch.id, &token)?;
        Ok(launch)
    }

    fn gates_check(&self, _: &Ctx, input: GateIn) -> Result<Guard> {
        let dir = input.project.get()?;
        let deny = config(Some(&input.project))?.config.gates.git.deny;
        // an alias is resolved by the real git outside the sandbox, so the agent cannot hide a denied command
        let is_alias = |sub: &str| {
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["config", "--get", &format!("alias.{sub}")])
                .output()
                .is_ok_and(|o| o.status.success())
        };
        Ok(Guard {
            refusal: legionctl_core::gates::git_refusal(&input.args, &deny, is_alias),
        })
    }

    fn config_show(&self, _: &Ctx, input: ProjectIn) -> Result<ConfigView> {
        let resolved = config(Some(&input.project))?;
        Ok(ConfigView {
            value: resolved.value,
            problems: resolved.problems,
        })
    }

    fn config_explain(&self, _: &Ctx, input: ExplainIn) -> Result<Explanation> {
        Ok(config(Some(&input.project))?.explain(&input.key))
    }

    fn config_layers(&self, _: &Ctx, input: ProjectIn) -> Result<LayerList> {
        let resolved = config(Some(&input.project))?;
        Ok(LayerList {
            layers: resolved.layers,
            locked: resolved.config.config.locked,
            problems: resolved.problems,
        })
    }
}
