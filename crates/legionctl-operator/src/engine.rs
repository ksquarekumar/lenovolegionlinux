//! The operations of `legionctl-api`, carried out as the user. Machine state is read here; machine changes go to
//! `legionctl-system` over D-Bus, which asks polkit. Every call that is not a read is recorded in the event log.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use legionctl_api::{
    BenchCompare, BenchCompareIn, BenchIn, BenchResult, BenchRun, BenchRuns, DossierAddIn,
    DossierAppendIn, DossierCheckIn, DossierEditIn, DossierFindings, DossierList, DossierReadIn,
    DossierSearchIn, DossierUseIn, DossierWriteIn, DryRunIn, Events, EventsIn, GateIn, Guard,
    GuardIn, HookReply, HookRunIn, InspectIn, KnobDetail, Knobs, Launch, Lines, Locks, MonitorIn,
    RecordIn, Recording, ReplayIn, Sample, Samples, SessionId, SessionIdIn, SessionIn,
    SessionStartIn, SessionState, Sessions, ToolCall, ToolCallRow, ToolCalls, ToolCallsIn, TuneIn,
    TuneReport, WindowsState,
};
use legionctl_api::{
    BranchIn, CapabilityList, Changes, CheckpointIn, CleanupIn, ClientPath, CloseIn, ConfigFiles,
    ConfigView, Ctx, DesktopGpu, Empty, Engine, EventIn, ExplainIn, Explanation, Findings,
    GpuPrimaryIn, GpuState, HookAddIn, HookBindIn, HookTable, IdIn, LayerList, LoadState,
    PowerApplyIn, PowerSource, ProfileIn, ProjectIn, ReminderAddIn, Reminders, RunEnv, RunEnvIn,
    SetupApplyIn, SetupReport, SkillList, Snapshot, StatusIn, Text, Validation, Workspace,
    WorkspaceIn, WorkspaceOpenIn, WorkspaceStates, Workspaces,
};
use legionctl_core::config::{Dirs, Resolved, resolve};

use crate::system1::System1Proxy;
use legionctl_api::{Manifest, PackageFetchIn, Packages};
use legionctl_core::db::Db;
use legionctl_core::dossier::{Dossiers, place_of};
use legionctl_core::packages::Store;
use legionctl_core::reminders::Event;
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
fn which(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| {
                m.is_file()
                    && std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0
            })
        })
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
    fn record(ctx: &Ctx, kind: &str, input: &impl serde::Serialize) -> Result<()> {
        Db::open_default()?.record(&ctx.actor, kind, &serde_json::to_value(input)?)?;
        Ok(())
    }

    /// A machine change through `legionctl-system`; polkit asks the human.
    fn system<T>(&self, call: impl AsyncFnOnce(&System1Proxy<'_>) -> zbus::Result<T>) -> Result<T> {
        self.runtime.block_on(async {
            let conn = zbus::Connection::system()
                .await
                .context("connecting to the system bus")?;
            let proxy = System1Proxy::new(&conn).await?;
            call(&proxy)
                .await
                .context("legionctl-system refused or failed")
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
        "no session id: set LEGIONCTL_SESSION (Claude Code sets CLAUDE_CODE_SESSION_ID)"
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
        match store.bind(session, "hook", &last, false, false, Some(&place)) {
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
             dossier_use (create: true for a new one; ask the user for the name if it is not clear), record the state \
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
         MCP tool dossier_use (create: true for a new one; ask the user for the name if it is not clear).{}",
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
         (or `/legionctl:dossier use <name> --create` for a new one), then send the prompt again.{}",
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

    fn power_apply(&self, ctx: &Ctx, input: PowerApplyIn) -> Result<Changes> {
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
        Self::record(ctx, "power.apply", &input)?;
        let name = serde_json::to_value(input.source)?;
        let name = name.as_str().unwrap_or("auto").to_string();
        Ok(Changes {
            lines: self.system(async move |p| p.apply(&name).await)?,
            dry_run: false,
        })
    }

    fn gpu_primary(&self, ctx: &Ctx, input: GpuPrimaryIn) -> Result<Changes> {
        let choice = match input.choice {
            DesktopGpu::Nvidia => "nvidia",
            DesktopGpu::Intel => "intel",
            DesktopGpu::Auto => "auto",
        };
        if input.dry_run {
            let plan = Plan::load(None)?;
            return Ok(dry_run(&gpu::set_desktop(&plan.machine(), choice)?));
        }
        Self::record(ctx, "gpu.primary", &input)?;
        Ok(Changes {
            lines: self.system(async move |p| p.set_desktop_gpu(choice).await)?,
            dry_run: false,
        })
    }

    fn cleanup(&self, ctx: &Ctx, input: CleanupIn) -> Result<Changes> {
        if input.dry_run {
            let plan = Plan::load(None)?;
            let opts = cleanup::Options {
                force: false,
                unload_modules: input.unload_modules,
            };
            return Ok(dry_run(&cleanup::plan(&plan.machine(), opts)?));
        }
        Self::record(ctx, "cleanup", &input)?;
        Ok(Changes {
            lines: self.system(async move |p| p.cleanup(input.unload_modules).await)?,
            dry_run: false,
        })
    }

    fn agents(&self, _: &Ctx, input: ProjectIn) -> Result<ConfigFiles> {
        Ok(agents::config_files(&home(), input.project.get()?))
    }

    fn workspace_open(&self, ctx: &Ctx, input: WorkspaceOpenIn) -> Result<Workspace> {
        Self::record(ctx, "workspace.open", &input)?;
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

    fn workspace_checkpoint(&self, ctx: &Ctx, input: CheckpointIn) -> Result<Lines> {
        Self::record(ctx, "workspace.checkpoint", &input)?;
        let store = dossiers()?;
        let ws = store.workspace_of(session(&input.session)?, path(input.repo.as_ref())?)?;
        Ok(Lines {
            lines: vec![store.checkpoint(&ws, &input.message)?],
        })
    }

    fn workspace_branch(&self, ctx: &Ctx, input: BranchIn) -> Result<Lines> {
        Self::record(ctx, "workspace.branch", &input)?;
        let store = dossiers()?;
        let ws = store.workspace_of(session(&input.session)?, path(input.repo.as_ref())?)?;
        Ok(Lines {
            lines: vec![format!(
                "on {}",
                store.switch_branch(&ws, input.topic.as_deref())?
            )],
        })
    }

    fn workspace_merge(&self, ctx: &Ctx, input: WorkspaceIn) -> Result<Lines> {
        Self::record(ctx, "workspace.merge", &input)?;
        let store = dossiers()?;
        let ws = store.workspace_of(session(&input.session)?, path(input.repo.as_ref())?)?;
        Ok(Lines {
            lines: store.merge(&ws)?,
        })
    }

    fn workspace_close(&self, ctx: &Ctx, input: CloseIn) -> Result<Lines> {
        Self::record(ctx, "workspace.close", &input)?;
        let store = dossiers()?;
        let ws = store.workspace_of(session(&input.session)?, path(input.repo.as_ref())?)?;
        Ok(Lines {
            lines: vec![store.close(&ws, input.force)?],
        })
    }

    fn setup_check(&self, _: &Ctx, _: Empty) -> Result<SetupReport> {
        Ok(setup::check())
    }

    fn setup_apply(&self, ctx: &Ctx, input: SetupApplyIn) -> Result<Changes> {
        Self::record(ctx, "setup.apply", &input)?;
        Ok(Changes {
            lines: setup::apply(input.install_ctx)?,
            dry_run: false,
        })
    }

    fn reminders_list(&self, _: &Ctx, input: ProjectIn) -> Result<Reminders> {
        Ok(reminders::list(&config(Some(&input.project))?))
    }

    fn reminders_on(&self, ctx: &Ctx, input: IdIn) -> Result<Reminders> {
        Self::record(ctx, "reminders.on", &input)?;
        reminders::set_enabled(&config(Some(&input.project))?, &input.id, true)?;
        Ok(reminders::list(&config(Some(&input.project))?))
    }

    fn reminders_off(&self, ctx: &Ctx, input: IdIn) -> Result<Reminders> {
        Self::record(ctx, "reminders.off", &input)?;
        reminders::set_enabled(&config(Some(&input.project))?, &input.id, false)?;
        Ok(reminders::list(&config(Some(&input.project))?))
    }

    fn reminders_add(&self, ctx: &Ctx, input: ReminderAddIn) -> Result<Reminders> {
        Self::record(ctx, "reminders.add", &input)?;
        reminders::add(
            &config(Some(&input.project))?,
            &input.id,
            &input.text,
            input.event,
        )?;
        Ok(reminders::list(&config(Some(&input.project))?))
    }

    fn reminders_rm(&self, ctx: &Ctx, input: IdIn) -> Result<Reminders> {
        Self::record(ctx, "reminders.rm", &input)?;
        reminders::remove(&config(Some(&input.project))?, &input.id)?;
        Ok(reminders::list(&config(Some(&input.project))?))
    }

    fn reminders_emit(&self, _: &Ctx, input: EventIn) -> Result<Text> {
        Ok(Text {
            text: reminders::emit(&config(Some(&input.project))?, input.event),
        })
    }

    fn hooks_list(&self, _: &Ctx, input: ProjectIn) -> Result<HookTable> {
        Ok(Self::hook_table(&config(Some(&input.project))?))
    }

    fn hooks_bind(&self, ctx: &Ctx, input: HookBindIn) -> Result<HookTable> {
        Self::record(ctx, "hooks.bind", &input)?;
        hooks::bind(&config(Some(&input.project))?, input.event, &input.action)?;
        Ok(Self::hook_table(&config(Some(&input.project))?))
    }

    fn hooks_unbind(&self, ctx: &Ctx, input: HookBindIn) -> Result<HookTable> {
        Self::record(ctx, "hooks.unbind", &input)?;
        hooks::unbind(&config(Some(&input.project))?, input.event, &input.action)?;
        Ok(Self::hook_table(&config(Some(&input.project))?))
    }

    fn hooks_add(&self, ctx: &Ctx, input: HookAddIn) -> Result<HookTable> {
        Self::record(ctx, "hooks.add", &input)?;
        hooks::add_command(
            &config(Some(&input.project))?,
            &input.id,
            &input.command,
            input.timeout,
        )?;
        Ok(Self::hook_table(&config(Some(&input.project))?))
    }

    fn hooks_rm(&self, ctx: &Ctx, input: IdIn) -> Result<HookTable> {
        Self::record(ctx, "hooks.rm", &input)?;
        hooks::remove_action(&config(Some(&input.project))?, &input.id)?;
        Ok(Self::hook_table(&config(Some(&input.project))?))
    }

    fn hooks_run(&self, _: &Ctx, input: HookRunIn) -> Result<HookReply> {
        let resolved = config(Some(&input.project))?;
        let store = Dossiers::open(&resolved.config.dossier)?;
        let session = input.session.as_ref().filter(|s| !s.0.is_empty());
        let mut active = match session {
            Some(s) => store.active(&s.0)?,
            None => None,
        };
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
        Self::record(ctx, "dossier.use", &input)?;
        let lines = dossiers()?.bind(
            session(&input.session)?,
            &ctx.actor,
            &input.name,
            input.create,
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

    fn dossier_release(&self, ctx: &Ctx, input: SessionIn) -> Result<Lines> {
        Self::record(ctx, "dossier.release", &input)?;
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

    fn dossier_check(&self, _: &Ctx, input: DossierCheckIn) -> Result<DossierFindings> {
        let ws = dossiers()?;
        let dossier = match input.dossier {
            Some(d) => d,
            None => ws
                .active(session(&input.session)?)?
                .context("this session has no active dossier; name one")?,
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
            "interval_ms must be 100-60000"
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

    fn packages_fetch(&self, ctx: &Ctx, input: PackageFetchIn) -> Result<Manifest> {
        Self::record(ctx, "packages.fetch", &input)?;
        let resolved = config(None)?;
        let package = resolved
            .config
            .packages
            .get(&input.name)
            .with_context(|| format!("no package {} in the config (packages)", input.name))?;
        Store::new(Store::default_root()).fetch(&input.name, package, input.force)
    }

    fn tools_record(&self, ctx: &Ctx, input: ToolCall) -> Result<Empty> {
        Db::open_default()?.record(&ctx.actor, "tool.call", &serde_json::to_value(&input)?)?;
        Ok(Empty {})
    }

    fn tools_calls(&self, _: &Ctx, input: ToolCallsIn) -> Result<ToolCalls> {
        Db::open_default()?
            .events_of("tool.call")?
            .into_iter()
            .map(|e| {
                Ok(ToolCallRow {
                    id: e.id,
                    ts: e.ts,
                    call: serde_json::from_value(e.payload)?,
                })
            })
            .filter(|row: &Result<ToolCallRow>| {
                row.as_ref().map_or(true, |r| {
                    input.session.as_ref().is_none_or(|s| *s == r.call.session)
                })
            })
            .collect()
    }

    fn windows_status(&self, _: &Ctx, _: Empty) -> Result<WindowsState> {
        windows::state()
    }

    fn windows_reboot(&self, ctx: &Ctx, input: DryRunIn) -> Result<Changes> {
        if input.dry_run {
            return Ok(dry_run(&windows::reboot_plan(&windows::state()?)?));
        }
        Self::record(ctx, "windows.reboot", &input)?;
        Ok(Changes {
            lines: self.system(async |p| p.reboot_to_windows().await)?,
            dry_run: false,
        })
    }

    fn record(&self, ctx: &Ctx, input: RecordIn) -> Result<Recording> {
        Self::record(ctx, "record", &input)?;
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

    fn tune(&self, ctx: &Ctx, input: TuneIn) -> Result<TuneReport> {
        let load = load::read_load(&SysRoot::from_env(), &config(None)?.config.load);
        anyhow::ensure!(
            input.force || load.level == load::Level::Idle,
            "the machine is not idle ({}); stop other work, or pass force",
            load.reasons.join("; ")
        );
        Self::record(ctx, "tune", &input)?;
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

    fn session_start(&self, ctx: &Ctx, input: SessionStartIn) -> Result<Launch> {
        Self::record(ctx, "session.start", &input)?;
        let dir = input.repo.get()?;
        let resolved = config(Some(&input.repo))?;
        let store = Dossiers::open(&resolved.config.dossier)?;
        let dossier = match input.dossier {
            Some(d) => d,
            None => store
                .last_in(&place_of(dir))?
                .context("no dossier was used in this repository yet; name one (and pass create for a new one)")?,
        };
        store.start_session(&input.agent, &dossier, input.create, dir, &resolved.config)
    }

    fn session_list(&self, _: &Ctx, _: Empty) -> Result<Sessions> {
        Ok(dossiers()?
            .sessions()?
            .into_iter()
            .map(|session| {
                // a scope that ended is gone from systemd: is-active then says inactive
                let unit = format!("legionctl-session-{}.scope", session.id);
                let out = Command::new("systemctl")
                    .args(["--user", "is-active", &unit])
                    .output();
                let state = out
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .ok()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "unknown".into());
                SessionState { state, session }
            })
            .collect())
    }

    fn session_stop(&self, ctx: &Ctx, input: SessionIdIn) -> Result<Lines> {
        Self::record(ctx, "session.stop", &input)?;
        let session = dossiers()?.session(&input.id)?;
        let unit = format!("legionctl-session-{}.scope", session.id);
        let out = Command::new("systemctl")
            .args(["--user", "stop", &unit])
            .output()?;
        anyhow::ensure!(
            out.status.success(),
            "stopping {unit}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        Ok(Lines {
            lines: vec![format!(
                "stopped session {} ({})",
                session.id, session.agent
            )],
        })
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
