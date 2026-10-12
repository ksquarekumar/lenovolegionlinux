//! The operations of `legionctl-api`, carried out as the user. Machine state is read here; machine changes go to
//! `legionctl-system` over D-Bus, which asks polkit. Every call that is not a read is recorded in the event log.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use legionctl_api::{
    CapabilityList, Changes, CleanupIn, ClientPath, ConfigFiles, Ctx, DesktopGpu, Empty, Engine,
    EventIn, Findings, GpuPrimaryIn, GpuState, HookAddIn, HookBindIn, HookTable, IdIn, LoadState,
    PowerApplyIn, PowerSource, ProfileIn, ProjectIn, ReminderAddIn, Reminders, RunEnv, RunEnvIn,
    SetupApplyIn, SetupReport, SkillList, Snapshot, StatusIn, Text, Validation, Worktree,
    WorktreeCreateIn, WorktreeListIn, WorktreeRmIn, Worktrees,
};
use legionctl_core::db::Db;
use legionctl_core::hooks::Hooks;
use legionctl_core::{agents, reminders, setup, skills, worktree};
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

    fn hooks<T>(ctx: &Ctx, f: impl FnOnce(&Hooks) -> Result<T>) -> Result<T> {
        let db = Db::open_default()?;
        let hooks = Hooks {
            db: &db,
            reminders: reminders::Store::default(),
            actor: &ctx.actor,
        };
        f(&hooks)
    }

    fn hook_table(hooks: &Hooks) -> Result<HookTable> {
        Ok(HookTable {
            actions: hooks.actions()?,
            bindings: hooks.bindings()?,
        })
    }
}

impl Engine for Operator {
    fn status(&self, _: &Ctx, input: StatusIn) -> Result<Snapshot> {
        let plan = Plan::load(path(input.profile.as_ref()))?;
        let detail = if input.fast {
            Detail::Fast
        } else {
            Detail::Full
        };
        Ok(Snapshot::read(&plan.root, &plan.profile, detail))
    }

    fn load(&self, _: &Ctx, _: Empty) -> Result<LoadState> {
        Ok(load::read_load(&SysRoot::from_env()))
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
        worktree::Store::default().list(input.sizes)
    }

    fn worktree_create(&self, ctx: &Ctx, input: WorktreeCreateIn) -> Result<Worktree> {
        Self::record(ctx, "worktree.create", &input)?;
        worktree::Store::default().create(&input.repo.0, &input.name, input.base.as_deref())
    }

    fn worktree_rm(&self, ctx: &Ctx, input: WorktreeRmIn) -> Result<Worktree> {
        Self::record(ctx, "worktree.rm", &input)?;
        worktree::Store::default().remove(&input.name, input.force)
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

    fn reminders_list(&self, _: &Ctx, _: Empty) -> Result<Reminders> {
        reminders::Store::default().list()
    }

    fn reminders_on(&self, ctx: &Ctx, input: IdIn) -> Result<Reminders> {
        Self::record(ctx, "reminders.on", &input)?;
        let store = reminders::Store::default();
        store.set_enabled(&input.id, true)?;
        store.list()
    }

    fn reminders_off(&self, ctx: &Ctx, input: IdIn) -> Result<Reminders> {
        Self::record(ctx, "reminders.off", &input)?;
        let store = reminders::Store::default();
        store.set_enabled(&input.id, false)?;
        store.list()
    }

    fn reminders_add(&self, ctx: &Ctx, input: ReminderAddIn) -> Result<Reminders> {
        Self::record(ctx, "reminders.add", &input)?;
        let store = reminders::Store::default();
        store.add(&input.id, &input.text, input.event)?;
        store.list()
    }

    fn reminders_rm(&self, ctx: &Ctx, input: IdIn) -> Result<Reminders> {
        Self::record(ctx, "reminders.rm", &input)?;
        let store = reminders::Store::default();
        store.remove(&input.id)?;
        store.list()
    }

    fn reminders_emit(&self, _: &Ctx, input: EventIn) -> Result<Text> {
        Ok(Text {
            text: reminders::Store::default().emit(input.event)?,
        })
    }

    fn hooks_list(&self, ctx: &Ctx, _: Empty) -> Result<HookTable> {
        Self::hooks(ctx, Self::hook_table)
    }

    fn hooks_bind(&self, ctx: &Ctx, input: HookBindIn) -> Result<HookTable> {
        Self::hooks(ctx, |h| {
            h.bind(input.event, &input.action)?;
            Self::hook_table(h)
        })
    }

    fn hooks_unbind(&self, ctx: &Ctx, input: HookBindIn) -> Result<HookTable> {
        Self::hooks(ctx, |h| {
            h.unbind(input.event, &input.action)?;
            Self::hook_table(h)
        })
    }

    fn hooks_add(&self, ctx: &Ctx, input: HookAddIn) -> Result<HookTable> {
        Self::hooks(ctx, |h| {
            h.add_command(&input.id, &input.command, input.timeout)?;
            Self::hook_table(h)
        })
    }

    fn hooks_rm(&self, ctx: &Ctx, input: IdIn) -> Result<HookTable> {
        Self::hooks(ctx, |h| {
            h.remove_action(&input.id)?;
            Self::hook_table(h)
        })
    }

    fn hooks_run(&self, ctx: &Ctx, input: EventIn) -> Result<Text> {
        Self::hooks(ctx, |h| {
            Ok(Text {
                text: h.run(input.event)?,
            })
        })
    }

    fn skills(&self, _: &Ctx, _: Empty) -> Result<SkillList> {
        skills::inventory(&home(), &reminders::Store::default())
    }
}
