//! The operations of `legionctl-api`, carried out as the user. Machine state is read here; machine changes go to
//! `legionctl-system` over D-Bus, which asks polkit. Every call that is not a read is recorded in the event log.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use legionctl_api::{
    CapabilityList, Changes, CleanupIn, ClientPath, ConfigFiles, ConfigView, Ctx, DesktopGpu,
    Empty, Engine, EventIn, ExplainIn, Explanation, Findings, GpuPrimaryIn, GpuState, HookAddIn,
    HookBindIn, HookTable, IdIn, LayerList, LoadState, PowerApplyIn, PowerSource, ProfileIn,
    ProjectIn, ReminderAddIn, Reminders, RunEnv, RunEnvIn, SetupApplyIn, SetupReport, SkillList,
    Snapshot, StatusIn, Text, Validation, Worktree, WorktreeCreateIn, WorktreeListIn, WorktreeRmIn,
    Worktrees,
};
use legionctl_api::{
    DossierAddIn, DossierAppendIn, DossierCheckIn, DossierEditIn, DossierFindings, DossierReadIn,
    DossierSearchIn, DossierUseIn, DossierWriteIn, Dossiers, Guard, GuardIn, HookReply, HookRunIn,
    Lines, Locks, SessionId, SessionIn,
};
use legionctl_core::config::{Dirs, Resolved, resolve};
use legionctl_core::db::Db;
use legionctl_core::dossier::Workspace;
use legionctl_core::reminders::Event;
use legionctl_core::{agents, hooks, reminders, setup, skills, worktree};
use legionctl_machine::ledger::Ledger;
use legionctl_machine::policy::{self, Machine, Options, Source};
use legionctl_machine::profile::{self, Profile};
use legionctl_machine::snapshot::Detail;
use legionctl_machine::sysroot::SysRoot;
use legionctl_machine::{cleanup, gpu, load};

#[zbus::proxy(
    default_service = "org.legionctl.System1",
    default_path = "/org/legionctl/System1",
    interface = "org.legionctl.System1"
)]
trait System1Methods {
    fn apply(&self, source: &str) -> zbus::Result<Vec<String>>;
    fn set_desktop_gpu(&self, choice: &str) -> zbus::Result<Vec<String>>;
    fn cleanup(&self, unload_modules: bool) -> zbus::Result<Vec<String>>;
}

pub struct Operator {
    /// Runs the D-Bus calls of machine changes; engine methods run on blocking threads.
    pub runtime: tokio::runtime::Handle,
}

fn has_command(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            std::fs::metadata(dir.join(name)).is_ok_and(|m| {
                m.is_file()
                    && std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0
            })
        })
    })
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

fn path(p: Option<&ClientPath>) -> Option<&Path> {
    p.map(|p| p.0.as_path())
}

/// The machine, its profile and the Fn+Q ledger, as the planners need them.
struct Plan {
    root: SysRoot,
    profile: Profile,
    ledger: Ledger,
}

impl Plan {
    fn load(profile: Option<&Path>) -> Result<Plan> {
        let root = SysRoot::from_env();
        Ok(Plan {
            ledger: Ledger::open(&root)?,
            profile: profile::load(profile)?.0,
            root,
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
        lines: steps.iter().filter_map(policy::describe).collect(),
        dry_run: true,
    }
}

impl Operator {
    fn record(ctx: &Ctx, kind: &str, input: &impl serde::Serialize) -> Result<()> {
        Db::open_default()?.record(&ctx.actor, kind, &serde_json::to_value(input)?)?;
        Ok(())
    }

    /// A machine change through `legionctl-system`; polkit asks the human.
    fn system(
        &self,
        call: impl AsyncFnOnce(&System1MethodsProxy<'_>) -> zbus::Result<Vec<String>>,
    ) -> Result<Changes> {
        let lines = self.runtime.block_on(async {
            let conn = zbus::Connection::system()
                .await
                .context("connecting to the system bus")?;
            let proxy = System1MethodsProxy::new(&conn).await?;
            call(&proxy)
                .await
                .context("legionctl-system refused or failed")
        })?;
        Ok(Changes {
            lines,
            dry_run: false,
        })
    }

    fn hook_table(config: &Resolved) -> HookTable {
        HookTable {
            actions: hooks::actions(config),
            bindings: hooks::bindings(config),
        }
    }
}

fn workspace() -> Result<Workspace> {
    Workspace::open(&config(None)?.config.dossier)
}

fn session(id: &SessionId) -> Result<&str> {
    anyhow::ensure!(
        !id.0.is_empty(),
        "no session id: set LEGIONCTL_SESSION (Claude Code sets CLAUDE_CODE_SESSION_ID)"
    );
    Ok(&id.0)
}

/// What a blocked prompt shows: why, and the commands that pass the gate, with the most recent dossiers.
fn gate_message(resolved: &Resolved) -> Result<String> {
    let recent: Vec<String> = Workspace::open(&resolved.config.dossier)?
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
    resolve(&Dirs::from_env(), path(project))
}

impl Engine for Operator {
    fn status(&self, _: &Ctx, input: StatusIn) -> Result<Snapshot> {
        let plan = Plan::load(path(input.profile.as_ref()))?;
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
        let plan = Plan::load(path(input.profile.as_ref()))?;
        Ok(Findings {
            findings: legionctl_machine::doctor::doctor(&plan.machine()),
        })
    }

    fn capabilities(&self, _: &Ctx, input: ProfileIn) -> Result<CapabilityList> {
        let (profile, _) = profile::load(path(input.profile.as_ref()))?;
        Ok(CapabilityList {
            profile_id: profile.id,
            capabilities: profile.capabilities,
        })
    }

    fn validate(&self, _: &Ctx, input: ProfileIn) -> Result<Validation> {
        let path =
            path(input.profile.as_ref()).map_or_else(profile::default_path, Path::to_path_buf);
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
        self.system(async move |p| p.apply(&name).await)
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
        self.system(async move |p| p.set_desktop_gpu(choice).await)
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
        self.system(async move |p| p.cleanup(input.unload_modules).await)
    }

    fn agents(&self, _: &Ctx, input: ProjectIn) -> Result<ConfigFiles> {
        Ok(agents::config_files(&home(), &input.project.0))
    }

    fn worktree_list(&self, _: &Ctx, input: WorktreeListIn) -> Result<Worktrees> {
        worktree::Store::new(config(None)?.config.worktree.root).list(input.sizes)
    }

    fn worktree_create(&self, ctx: &Ctx, input: WorktreeCreateIn) -> Result<Worktree> {
        Self::record(ctx, "worktree.create", &input)?;
        worktree::Store::new(config(None)?.config.worktree.root).create(
            &input.repo.0,
            &input.name,
            input.base.as_deref(),
        )
    }

    fn worktree_rm(&self, ctx: &Ctx, input: WorktreeRmIn) -> Result<Worktree> {
        Self::record(ctx, "worktree.rm", &input)?;
        worktree::Store::new(config(None)?.config.worktree.root).remove(&input.name, input.force)
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
        let session = input.session.as_ref().filter(|s| !s.0.is_empty());
        let active = match session {
            Some(s) => Workspace::open(&resolved.config.dossier)?.active(&s.0)?,
            None => None,
        };
        // the gate: a session without a dossier gets no prompt through, except the one that binds a dossier
        let runs_dossier = input
            .prompt
            .as_deref()
            .is_some_and(|p| p.contains("legionctl:dossier"));
        if input.event == Event::Prompt
            && resolved.config.dossier.gate
            && session.is_some()
            && active.is_none()
            && !runs_dossier
        {
            return Ok(HookReply {
                text: String::new(),
                block: Some(gate_message(&resolved)?),
            });
        }
        // before the reply ends, the agent (not the user) hears what is not set up, and sets it up first
        if input.event == Event::Stop && session.is_some() && active.is_none() {
            return Ok(HookReply {
                text: String::new(),
                block: Some(
                    "legionctl: this session has no dossier. Before you end this reply, bind one with the \
                     legionctl MCP tool dossier_use (create: true for a new one; ask the user for the name if it is \
                     not clear), record the state in its plan.md and MEMORY, then finish."
                        .into(),
                ),
            });
        }
        let mut text = hooks::run(&resolved, input.event);
        if input.event == Event::SessionStart
            && let Some(s) = session
        {
            let line = match &active {
                Some(d) => format!("legionctl dossier: {d} (session {})", s.0),
                None if resolved.config.dossier.gate => {
                    "legionctl dossier: none; prompts are blocked until `/legionctl:dossier use <name>` (add --create for a new one)".into()
                }
                None => "legionctl dossier: none".into(),
            };
            text = if text.is_empty() {
                line
            } else {
                format!("{line}\n{text}")
            };
        }
        Ok(HookReply { text, block: None })
    }

    fn dossier_use(&self, ctx: &Ctx, input: DossierUseIn) -> Result<Lines> {
        Self::record(ctx, "dossier.use", &input)?;
        let lines = workspace()?.bind(
            session(&input.session)?,
            &ctx.actor,
            &input.name,
            input.create,
            input.steal,
        )?;
        Ok(Lines { lines })
    }

    fn dossier_active(&self, _: &Ctx, input: SessionIn) -> Result<Lines> {
        let id = session(&input.session)?;
        let active = workspace()?.active(id)?;
        Ok(Lines {
            lines: vec![format!(
                "session {id}: active dossier {}",
                active.as_deref().unwrap_or("(none)")
            )],
        })
    }

    fn dossier_release(&self, ctx: &Ctx, input: SessionIn) -> Result<Lines> {
        Self::record(ctx, "dossier.release", &input)?;
        let released = workspace()?.release(session(&input.session)?)?;
        Ok(Lines {
            lines: vec![
                released.map_or_else(|| "no active dossier".into(), |d| format!("released {d}")),
            ],
        })
    }

    fn dossier_locks(&self, _: &Ctx, _: Empty) -> Result<Locks> {
        workspace()?.locks()
    }

    fn dossier_ls(&self, _: &Ctx, _: Empty) -> Result<Dossiers> {
        Ok(workspace()?.list())
    }

    fn dossier_read(&self, _: &Ctx, input: DossierReadIn) -> Result<Lines> {
        let lines = workspace()?.read(
            &input.path,
            input.first as usize,
            input.last.map(|l| l as usize),
        )?;
        Ok(Lines { lines })
    }

    fn dossier_search(&self, _: &Ctx, input: DossierSearchIn) -> Result<Lines> {
        let lines = workspace()?.search(
            &input.pattern,
            input.dossier.as_deref(),
            input.ignore_case,
            input.limit as usize,
        );
        Ok(Lines { lines })
    }

    fn dossier_write(&self, _: &Ctx, input: DossierWriteIn) -> Result<Lines> {
        let path = workspace()?.write(
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
        let path = workspace()?.append(session(&input.session)?, &input.path, &input.content.0)?;
        Ok(Lines {
            lines: vec![path.display().to_string()],
        })
    }

    fn dossier_edit(&self, _: &Ctx, input: DossierEditIn) -> Result<Lines> {
        let lines = workspace()?.edit(
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
        let rel = workspace()?.add(
            session(&input.session)?,
            &input.class,
            &input.slug,
            &input.content.0,
        )?;
        Ok(Lines { lines: vec![rel] })
    }

    fn dossier_check(&self, _: &Ctx, input: DossierCheckIn) -> Result<DossierFindings> {
        let ws = workspace()?;
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
            refusal: workspace()?
                .write_refusal(session(&input.session)?, Path::new(&input.path))?,
        })
    }

    fn skills(&self, _: &Ctx, input: ProjectIn) -> Result<SkillList> {
        Ok(skills::inventory(&home(), &config(Some(&input.project))?))
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
