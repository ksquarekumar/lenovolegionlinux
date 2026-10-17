//! The operations of `legionctl-api`, carried out as the user. Machine state is read here; machine changes go to
//! `legionctl-system` over D-Bus, which asks polkit. Every call that is not a read is recorded in the event log.

use std::path::{Path, PathBuf};
use std::process::Command;

use legionctl_machine::policy::which;

use anyhow::{Context, Result};
use legionctl_api::{
    Account, BenchCompare, BenchCompareIn, BenchIn, BenchRun, Code, DossierAddIn, DossierAppendIn,
    DossierCheckIn, DossierEditIn, DossierFindings, DossierList, DossierMigrateIn, DossierReadIn,
    DossierReleaseIn, DossierRenameIn, DossierSearchIn, DossierUseIn, DossierWriteIn, DryRunIn,
    Events, EventsIn, GateIn, Guard, GuardIn, HookReply, HookRunIn, InspectIn, IntoWire,
    KnobDetail, Knobs, Launch, Lines, Locks, MonitorIn, Readings, RecordIn, Recording, ReplayIn,
    Samples, SensorsIn, SessionExec, SessionId, SessionIdIn, SessionIn, SessionResumeIn,
    SessionStartIn, SessionState, Sessions, ToolCall, ToolCallRow, ToolCallsIn, TuneIn, TuneReport,
    WindowsState, code_of, coded,
};
use legionctl_api::{
    AccountIn, AccountLoginIn, CapabilityList, Changes, CheckpointIn, CleanupIn, ClientPath,
    CloseIn, ConfigFiles, ConfigView, Ctx, Empty, Engine, ExplainIn, Explanation, Findings,
    GpuPrimaryIn, GpuState, HookAddIn, HookBindIn, HookTable, IdIn, LayerList, LoadState, MergeIn,
    PowerApplyIn, PowerSource, ProfileIn, ProjectIn, ReminderAddIn, Reminders, RunEnv, RunEnvIn,
    RunnableGrant, RunnablesCheck, RunnablesCheckIn, RunnablesShowIn, ServiceIn, SetupApplyIn,
    SetupReport, SkillList, Snapshot, StatusIn, Validation, Workspace, WorkspaceIn,
    WorkspaceOpenIn, WorkspaceStatus, Workspaces,
};
use legionctl_api::{
    Autostart, AutostartIn, DisplayFixIn, FanCurveIn, FanState, FirmwareDevices, FirmwareUpdateIn,
    GpuGiveIn, GpuName, GpuOwner, GpuPowerIn, KnobHoldIn, KnobIn, OnOff, OverlayIn, PackIn,
    PackKind, RgbState, Switch, SwitchIn, UndoIn, VfioBindIn, VfioDevices, VfioIn,
    WindowsVmBeginIn, WindowsVmBegun,
};
use legionctl_api::{
    CachePurgeIn, CacheStat, CachedCall, EventRow, JobIdIn, Page, PageIn, Principal,
    SessionActivityIn,
};
use legionctl_api::{
    CheckShowIn, ChecksRunIn, MutantRecordIn, TestDetail, TestReview, TestReviewIn, TestRow,
    TestShowIn, TestsRunIn,
};
use legionctl_core::archetypes::Sources;
use legionctl_core::cache::Cache;
use legionctl_core::codetests::Side;
use legionctl_core::config::{Dirs, Resolved, resolve};

use crate::rules;
use crate::system1::System1Proxy;
use legionctl_api::{
    AgentName, AgentSetIn, AgentUsageIn, CoreClass, DossierGraphIn, DossierNudgesIn, Edit,
    GatewayUsage, GatewayUsageIn, Graph, Plugin, PluginInstallIn, SettingsScope, UsageReport,
    VirtCreateIn, VirtTuneIn,
};
use legionctl_api::{
    CacheGetIn, CachePutIn, CacheValue, Manifest, PackageFetchIn, Packages, RunOutput, SessionRunIn,
};
use legionctl_api::{
    Conditions, PlanView, RuleDetail, RuleEditIn, RuleIn, RuleResetIn, RuleState, Rules,
};
use legionctl_api::{ImagesScanIn, RepoIn, Scan, ServiceState, VirtActIn, VirtStatus};
use legionctl_core::db::{Db, EventFilter};
use legionctl_core::dossier::{Dossiers, place_of};
use legionctl_core::packages::Store;
use legionctl_core::reminders::Event;
use legionctl_core::session::Start;
use legionctl_core::workspace::Place;
use legionctl_core::{agents, hooks, reminders, setup, skills};
use legionctl_machine::configset::{self, Folder, Layer};
use legionctl_machine::graphics::{Finding, Fix};
use legionctl_machine::ledger::Ledger;
use legionctl_machine::policy::{self, Machine, Options, Source};
use legionctl_machine::profile::{self, Profile};
use legionctl_machine::snapshot::Detail;
use legionctl_machine::sysroot::SysRoot;
use legionctl_machine::telemetry::{self, RootSample};
use legionctl_machine::{bench, cleanup, fan, fwupd, gpu, knobs, load, rgb, sensors, windows};

pub struct Operator {
    /// Runs the D-Bus calls of machine changes; engine methods run on blocking threads.
    pub runtime: tokio::runtime::Handle,
    /// The telemetry ring: the last [`RING`] samples of `monitor`, `sensors` and `/v1/metrics`, oldest first.
    pub ring: std::sync::Mutex<std::collections::VecDeque<telemetry::Sample>>,
    /// The vitals overlay is on (`overlay`); the session module draws it.
    pub overlay: std::sync::atomic::AtomicBool,
}

/// Samples the telemetry ring keeps (10 minutes of `sensors --watch`).
const RING: usize = 600;

fn has_command(name: &str) -> bool {
    which(name).is_some()
}

/// The `GSettings` schema of the fonts' antialiasing.
const INTERFACE: &str = "org.gnome.desktop.interface";

/// Run `gsettings` (the user's dconf); its output, trimmed (`get` gives the value as `GVariant` text).
fn gsettings(args: &[&str]) -> Result<String> {
    let out = Command::new("gsettings")
        .args(args)
        .output()
        .context("running gsettings")?;
    anyhow::ensure!(
        out.status.success(),
        "gsettings {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// What a user-level change changes, as `display fix` and `undo` show it.
fn target(fix: &Fix) -> String {
    match fix {
        Fix::File { path, .. } => path.display().to_string(),
        Fix::Setting { key, .. } => format!("{INTERFACE} {key}"),
        Fix::Mode { connector, .. } => format!("{connector} mode"),
    }
}

/// The value a user-level change sets.
fn value(fix: &Fix) -> String {
    match fix {
        Fix::File { content, .. } => content
            .as_ref()
            .map_or_else(|| "no file".into(), |c| format!("{c:?}")),
        Fix::Setting { value, .. } => value.clone(),
        Fix::Mode { mode, .. } => mode.clone(),
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn path(p: Option<&ClientPath>) -> Result<Option<&Path>> {
    p.map(ClientPath::get).transpose()
}

/// The cache key of a tool call: the caller's session (a sandbox's entries are its own), the server, the tool, the
/// arguments and, for a tool that reads the working copy, its git state.
fn tool_key(ctx: &Ctx, call: &CachedCall) -> Result<String> {
    use blake2::{Blake2b, Digest, digest::consts::U16};
    let scope = match &ctx.principal {
        Principal::User => String::new(),
        Principal::Session(id) => id.clone(),
    };
    let state = match &call.workspace {
        Some(dir) => legionctl_core::workspace::git_state(dir.get()?)?,
        None => String::new(),
    };
    let input = serde_json::to_vec(&(scope, &call.server, &call.tool, &call.args, state))?;
    Ok(format!("{:x}", Blake2b::<U16>::digest(&input)))
}

/// The byte limit of a cache namespace (`cache.<ns>.max_bytes`).
fn cache_limit(ns: &str) -> Result<u64> {
    config(None)?
        .config
        .cache
        .get(ns)
        .map(|l| l.max_bytes)
        .with_context(|| format!("no limit for the cache namespace {ns} (cache.{ns}.max_bytes)"))
}

/// Build each service of the repository that its compose file or Dockerfiles build (`images build`).
fn build_images(repo: &Path) -> Result<Lines> {
    let (project, tag) = (
        legionctl_core::images::project_name(repo),
        legionctl_core::images::tag(repo)?,
    );
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

/// The stop signals of the headless runs in progress, by job: the session policy offloads a run with it.
/// Held while `dossier migrate` runs: one run at a time, and no write into the old root meanwhile.
static MIGRATING: std::sync::Mutex<()> = std::sync::Mutex::new(());

static STOPS: std::sync::Mutex<std::collections::BTreeMap<i64, tokio::sync::oneshot::Sender<()>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Register a stop signal for the run of `job`.
fn stoppable(job: i64) -> tokio::sync::oneshot::Receiver<()> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    STOPS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(job, tx);
    rx
}

/// Stop the headless run of `job`; false when it does not run here.
pub fn stop_run(job: i64) -> bool {
    STOPS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&job)
        .is_some_and(|tx| tx.send(()).is_ok())
}

/// Record how a job ended.
pub(crate) fn end_job<T: serde::Serialize>(db: &Db, job: i64, out: &Result<T>) -> Result<()> {
    let outcome = match out {
        Ok(value) => Ok(serde_json::to_value(value)?),
        Err(e) => Err(format!("{e:#}")),
    };
    db.job_end(job, &outcome)
}

/// The thread part of a hook call of `session`: a stop's block (and the wakes it starts), the prompt's messages, and
/// at a resumed session start the teammates that Claude Code lost (they run as operator sessions now).
fn thread_hooks(
    store: &Dossiers,
    session: &str,
    input: &HookRunIn,
) -> Result<(Option<String>, Vec<String>)> {
    Ok(match Event::from(input.event) {
        Event::Stop => {
            let block = store.thread_stop(session, false)?;
            crate::thread::wake_due();
            (block, Vec::new())
        }
        Event::Prompt => (None, store.thread_prompt(session)?.into_iter().collect()),
        Event::SessionStart if input.source.as_deref() == Some("resume") => {
            let lost = store.lead_resumed(session)?;
            crate::thread::wake_due();
            (None, lost)
        }
        Event::SessionStart => (None, Vec::new()),
    })
}

/// Before the migration no dossier operation runs, and a session goes on: one line, never a block.
fn unmigrated(event: Event) -> Option<HookReply> {
    legionctl_core::migrate::pending().then(|| HookReply {
        text: match event {
            Event::SessionStart | Event::Prompt => legionctl_core::migrate::NOT_MIGRATED.into(),
            Event::Stop => String::new(),
        },
        block: None,
    })
}

/// The path of the knob `knob` (an id or a path) on this machine.
fn knob_path(profile: Option<&ClientPath>, knob: &str) -> Result<String> {
    let plan = Plan::load(path(profile)?)?;
    Ok(knobs::find(&plan.machine(), knob)?
        .ok_or_else(|| {
            coded(
                Code::NotFound,
                format!("no knob {knob} on this machine (see legionctl knobs list)"),
            )
        })?
        .path)
}

/// The conditions the layers are tested against now.
fn machine_conditions(m: &Machine) -> Result<legionctl_machine::configset::Conditions> {
    m.conditions(
        m.source()?,
        m.ledger
            .get(legionctl_machine::ledger::Key::Override)?
            .as_deref(),
    )
}

/// The conditions the layers are tested against now.
fn conditions(m: &Machine) -> Result<Conditions> {
    let c = machine_conditions(m)?;
    Ok(Conditions {
        power: c.power.name().into(),
        fnq: c.fnq,
        workload: c.workload.into_iter().collect(),
        kernel: c.kernel,
    })
}

/// A layer with its id and whether it applies now.
fn rule_state(m: &Machine, layer: &Layer) -> Result<RuleState> {
    let c = machine_conditions(m)?;
    Ok(RuleState {
        id: layer.id(),
        active: layer.off.is_none() && layer.applies(&c),
        layer: layer.clone().into_wire(),
    })
}

/// The event id a page cursor stands for (0 without one), and the page length.
fn page_of(cursor: Option<&str>, limit: u32) -> Result<(i64, u32)> {
    if !(1..=1000).contains(&limit) {
        return Err(coded(Code::InvalidInput, "--limit must be 1-1000"));
    }
    let after = match cursor {
        None => 0,
        Some(c) => c.parse().map_err(|_| {
            coded(
                Code::InvalidInput,
                format!("cursor {c} is not one this operator gave"),
            )
        })?,
    };
    Ok((after, limit))
}

/// Rows of the log as a wire page.
fn page<T: IntoWire<W>, W>((rows, next): (Vec<T>, Option<i64>)) -> Page<W> {
    Page {
        items: rows.into_iter().map(IntoWire::into_wire).collect(),
        next: next.map(|n| n.to_string()),
    }
}

pub(crate) fn bench_run_of(e: legionctl_core::db::EventRow) -> Result<BenchRun> {
    Ok(BenchRun {
        id: e.id,
        at: e.ts,
        result: serde_json::from_value::<bench::BenchResult>(e.payload)?.into_wire(),
    })
}

/// `systemctl --user <verb>` on a session's scope (stop, freeze, thaw).
fn session_control(what: legionctl_core::session::Control, id: &str, done: &str) -> Result<Lines> {
    let session = dossiers()?.session(id)?;
    legionctl_core::session::control(&session.id, session.inside, what)?;
    Ok(Lines {
        lines: vec![format!("session {} ({}) {done}", session.id, session.agent)],
    })
}

/// The agent layer of a repository's image (W5, note 18): git and, for the agents, Node.js from the image's own
/// package manager; the agents and their adapters from npm; the static client as `legionctl` and as the git gate,
/// with the real git kept for it.
fn agent_layer(base: &str, npm: &[String]) -> String {
    let node = if npm.is_empty() { "" } else { " nodejs npm" };
    let mut text = format!(
        "FROM {base}\n\
         USER root\n\
         RUN set -e; \\\n\
         if command -v apt-get >/dev/null; then apt-get update && apt-get install -y --no-install-recommends git ca-certificates{node} && rm -rf /var/lib/apt/lists/*; \\\n\
         elif command -v apk >/dev/null; then apk add --no-cache git ca-certificates{node}; \\\n\
         elif command -v dnf >/dev/null; then dnf install -y git ca-certificates{node} && dnf clean all; \\\n\
         elif command -v pacman >/dev/null; then pacman -Sy --noconfirm git ca-certificates{node} && pacman -Scc --noconfirm; \\\n\
         else echo 'the image has none of apt-get, apk, dnf and pacman, so legionctl cannot add git' >&2; exit 1; fi\n"
    );
    if !npm.is_empty() {
        text.push_str("RUN npm install -g --no-fund --no-audit ");
        text.push_str(&npm.join(" "));
        text.push('\n');
    }
    text.push_str(
        "COPY legionctl /usr/local/bin/legionctl\n\
         RUN mkdir -p /usr/local/libexec/legionctl && mv \"$(command -v git)\" /usr/local/libexec/legionctl/git \\\n\
         && ln -s /usr/local/bin/legionctl /usr/bin/git\n\
         ENV LEGIONCTL_REAL_GIT=/usr/local/libexec/legionctl/git\n\
         ENTRYPOINT []\n",
    );
    text
}

/// Make the agent image of the repository that holds `dir`: its service's image (built with `images build`), else
/// the configured base, plus the agent layer.
fn pack(
    dir: &Path,
    service: Option<&str>,
    container: &legionctl_core::config::ContainerConfig,
) -> Result<Lines> {
    let repo = place_of(dir);
    let built: Vec<String> = legionctl_core::images::scan_repository(&repo)?
        .services
        .into_iter()
        .filter(|(_, s)| s.build.is_some())
        .map(|(name, _)| name)
        .collect();
    let chosen = match (service, built.as_slice()) {
        (None, []) => None,
        (Some(name), _) => {
            anyhow::ensure!(
                built.iter().any(|b| b == name),
                "no service {name} with a Dockerfile; the repository builds {built:?}"
            );
            Some(name.to_string())
        }
        (None, [one]) => Some(one.clone()),
        (None, many) => anyhow::bail!(
            "the repository builds {many:?}: choose the one for the agent with --service <name>"
        ),
    };
    let mut lines = Vec::new();
    let base = match chosen {
        Some(name) => {
            lines.extend(build_images(&repo)?.lines);
            let (project, tag) = (
                legionctl_core::images::project_name(&repo),
                legionctl_core::images::tag(&repo)?,
            );
            legionctl_core::images::image(&project, &name, &tag)
        }
        None => container.base.clone(),
    };
    let context = legionctl_core::cache::default_path()
        .with_file_name("pack")
        .join(legionctl_core::images::project_name(&repo));
    std::fs::create_dir_all(&context)?;
    std::fs::copy(&container.client, context.join("legionctl")).with_context(|| {
        format!(
            "no static legionctl at {} (session.container.client): the package installs it; in a checkout, `make static`",
            container.client
        )
    })?;
    std::fs::write(
        context.join("Dockerfile"),
        agent_layer(&base, &container.npm),
    )?;
    let image = legionctl_core::session::agent_image(&repo);
    let out = Command::new("docker")
        .args(["buildx", "build", "--load", "-t", &image])
        .arg(&context)
        .output()
        .context("running docker buildx")?;
    anyhow::ensure!(
        out.status.success(),
        "building {image}: {}",
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .rev()
            .take(15)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
    );
    lines.push(format!("packed {image} from {base}"));
    Ok(Lines { lines })
}

/// `command`'s stdout, or its stderr as the error.
pub(crate) fn output(command: &mut Command) -> Result<Vec<u8>> {
    let out = command
        .output()
        .with_context(|| format!("running {}", command.get_program().display()))?;
    anyhow::ensure!(
        out.status.success(),
        "{}: {}",
        command.get_program().display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(out.stdout)
}

/// The cloud image at `url`, downloaded once next to the VM disks and checked against its folder's `SHA512SUMS`.
fn vm_base(url: &str) -> Result<PathBuf> {
    let (folder, file) = url
        .rsplit_once('/')
        .context("session.vm.base is a URL of a file")?;
    let path = legionctl_core::session::vm_root().join(format!(
        "base-{:016x}-{file}",
        legionctl_machine::fnv(url.as_bytes())
    ));
    if path.is_file() {
        return Ok(path);
    }
    std::fs::create_dir_all(legionctl_core::session::vm_root())?;
    let part = path.with_extension("part");
    output(
        Command::new("curl")
            .args(["-fsSL", "--retry", "3", "-o"])
            .arg(&part)
            .arg(url),
    )?;
    let sums = output(
        Command::new("curl")
            .args(["-fsSL", "--retry", "3"])
            .arg(format!("{folder}/SHA512SUMS")),
    )?;
    let want = String::from_utf8_lossy(&sums)
        .lines()
        .find_map(|l| {
            let (hash, name) = l.split_once(char::is_whitespace)?;
            (name.trim().trim_start_matches('*') == file).then(|| hash.to_string())
        })
        .with_context(|| format!("{folder}/SHA512SUMS has no {file}"))?;
    let got = output(Command::new("sha512sum").arg(&part))?;
    anyhow::ensure!(
        String::from_utf8_lossy(&got).split_whitespace().next() == Some(want.as_str()),
        "{url} does not match its SHA512SUMS"
    );
    std::fs::rename(&part, &path)?;
    Ok(path)
}

/// cloud-init's user data for a repository's disk: git, Node.js and the agents, the static client as `legionctl`,
/// the git gate and the guest service; then a mark on the serial console, and power off.
fn vm_user_data(npm: &[String], gpu: bool) -> String {
    let node = if npm.is_empty() { "" } else { ", nodejs, npm" };
    // NVIDIA's own repository (Debian's nvidia-open 550 does not build on its current kernel, 2026-10-14); its DKMS
    // module needs the generic kernel (the cloud kernel has no DRM)
    let nvidia = if gpu {
        "cd /tmp; curl -fsSLO https://developer.download.nvidia.com/compute/cuda/repos/debian13/x86_64/cuda-keyring_1.1-1_all.deb; \
         dpkg -i cuda-keyring_1.1-1_all.deb; apt-get update; DEBIAN_FRONTEND=noninteractive apt-get install -y \
         linux-image-amd64 linux-headers-amd64 nvidia-open || \
         { tail -n 60 /var/lib/dkms/nvidia*/*/build/make.log; false; }; \
         DEBIAN_FRONTEND=noninteractive apt-get purge -y \"linux-image-*-cloud-amd64\" linux-image-cloud-amd64; \
         update-grub; ls /lib/modules/*/updates/dkms/nvidia.ko*; cd /; "
    } else {
        ""
    };
    let agents = if npm.is_empty() {
        String::new()
    } else {
        format!("npm install -g --no-fund --no-audit {}; ", npm.join(" "))
    };
    format!(
        "#cloud-config\n\
         package_update: true\n\
         packages: [git, ca-certificates{node}]\n\
         runcmd:\n  \
         - [sh, -ec, '{nvidia}mount -o ro LABEL=cidata /mnt; install -m 755 /mnt/legionctl /usr/local/bin/legionctl; \
         install -m 644 /mnt/legionctl-guest.service /etc/systemd/system/; umount /mnt; \
         mkdir -p /usr/local/libexec/legionctl; mv \"$(command -v git)\" /usr/local/libexec/legionctl/git; \
         ln -s /usr/local/bin/legionctl /usr/bin/git; {agents}\
         systemctl enable legionctl-guest.service; systemctl mask serial-getty@hvc0.service; \
         touch /etc/cloud/cloud-init.disabled; echo legionctl-pack-ok > /dev/ttyS0']\n\
         power_state: {{mode: poweroff, timeout: 30, condition: true}}\n"
    )
}

const GUEST_UNIT: &str = "[Unit]\nDescription=legionctl session (W5)\nAfter=network-online.target\n\
Wants=network-online.target\n\n[Service]\nType=simple\nExecStart=/usr/local/bin/legionctl guest\n\
StandardOutput=tty\nStandardError=tty\nTTYPath=/dev/ttyS0\n\n[Install]\nWantedBy=multi-user.target\n";

/// Make the VM disk of the repository that holds `dir`: a layer over the base cloud image, set up once by
/// cloud-init in a first boot.
fn pack_vm(
    dir: &Path,
    container: &legionctl_core::config::ContainerConfig,
    vm: &legionctl_core::config::VmConfig,
    gpu: bool,
) -> Result<Lines> {
    let base = vm_base(&vm.base)?;
    let disk = legionctl_core::session::vm_disk(dir);
    let seed = disk.with_extension("seed");
    let _ = std::fs::remove_dir_all(&seed);
    std::fs::create_dir_all(&seed)?;
    std::fs::copy(&container.client, seed.join("legionctl")).with_context(|| {
        format!(
            "no static legionctl at {} (session.container.client): the package installs it; in a checkout, `make static`",
            container.client
        )
    })?;
    std::fs::write(seed.join("legionctl-guest.service"), GUEST_UNIT)?;
    std::fs::write(seed.join("user-data"), vm_user_data(&container.npm, gpu))?;
    std::fs::write(
        seed.join("meta-data"),
        "instance-id: legionctl-pack\nlocal-hostname: legionctl\n",
    )?;
    let iso = disk.with_extension("seed.iso");
    output(
        Command::new("xorriso")
            .args(["-as", "mkisofs", "-quiet", "-V", "cidata", "-J", "-r", "-o"])
            .arg(&iso)
            .arg(&seed),
    )?;
    let new = disk.with_extension("new.qcow2");
    output(
        Command::new("qemu-img")
            .args(["create", "-q", "-f", "qcow2", "-F", "qcow2", "-b"])
            .arg(&base)
            .arg(&new)
            .arg(format!("{}G", vm.disk_gib)),
    )?;
    // the NVIDIA module is a large compile
    first_boot(&disk, &new, (&iso, if gpu { "4096" } else { "2048" }))?;
    std::fs::rename(&new, &disk)?;
    let mark = legionctl_core::session::vm_gpu_mark(&disk);
    if gpu {
        std::fs::write(&mark, "")?;
    } else {
        let _ = std::fs::remove_file(&mark);
    }
    std::fs::remove_file(&iso)?;
    std::fs::remove_dir_all(&seed)?;
    Ok(Lines {
        lines: vec![format!(
            "packed {} from {}{}",
            disk.display(),
            vm.base,
            if gpu { " with the NVIDIA driver" } else { "" }
        )],
    })
}

/// The first boot of a new VM disk `new` with the cloud-init seed `iso`: it must end with the mark on the console.
fn first_boot(disk: &Path, new: &Path, (iso, memory): (&Path, &str)) -> Result<()> {
    let log = disk.with_extension("pack.log");
    let mut first_boot = Command::new("qemu-system-x86_64")
        .args([
            "-machine",
            "q35,accel=kvm,smm=off",
            "-cpu",
            "host",
            "-m",
            memory,
            "-smp",
            "2",
        ])
        .args(["-nodefaults", "-display", "none", "-monitor", "none"])
        .arg("-serial")
        .arg(format!("file:{}", log.display()))
        .arg("-drive")
        .arg(format!("file={},if=virtio", new.display()))
        .arg("-drive")
        .arg(format!(
            "file={},if=virtio,format=raw,readonly=on",
            iso.display()
        ))
        .args([
            "-netdev",
            "user,id=net",
            "-device",
            "virtio-net-pci,netdev=net",
        ])
        .stdin(std::process::Stdio::null())
        .spawn()
        .context("running qemu-system-x86_64")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_mins(20);
    let status = loop {
        if let Some(status) = first_boot.try_wait()? {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = first_boot.kill();
            anyhow::bail!(
                "the first boot of {} took over 20 min; see {}",
                disk.display(),
                log.display()
            );
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    };
    let console = std::fs::read_to_string(&log).unwrap_or_default();
    anyhow::ensure!(
        status.success() && console.contains("legionctl-pack-ok"),
        "the first boot did not set up the disk ({status}); see {}",
        log.display()
    );
    Ok(())
}

/// QMP of the running Windows VM.
/// The error says that this operator has no desktop session: no session bus, or no Mutter on it.
fn no_desktop(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        matches!(
            c.downcast_ref::<zbus::Error>(),
            Some(zbus::Error::Address(_) | zbus::Error::InputOutput(_) | zbus::Error::Connection(..))
        ) || matches!(
            c.downcast_ref::<zbus::Error>(),
            Some(zbus::Error::MethodError(name, _, _)) if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown"
        )
    })
}

pub(crate) fn windows_qmp() -> Result<legionctl_core::qmp::Qmp> {
    legionctl_core::qmp::Qmp::connect(&legionctl_core::session::windows_root().join("qmp.sock"))
        .context("no Windows VM runs (legionctl windows vm start)")
}

/// The NVIDIA GPU's IOMMU group and who has it: Linux (its driver), Windows (on vfio-pci and in the running VM), or
/// no one (on vfio-pci).
pub(crate) fn nvidia_owner(root: &SysRoot) -> Result<(Vec<String>, GpuOwner)> {
    let group = legionctl_machine::vfio::nvidia_group(root)?;
    let in_vm = windows_has_gpu(&legionctl_core::session::windows_root().join("qmp.sock"))?;
    let owner = match legionctl_machine::vfio::driver(root, &group[0])?.as_deref() {
        Some("vfio-pci") if in_vm => GpuOwner::Windows,
        Some("vfio-pci") => GpuOwner::None,
        _ => GpuOwner::Linux,
    };
    Ok((group, owner))
}

/// Whether the running Windows VM (QMP at `socket`) has the GPU (`gpu give nvidia windows` added `gpu0`); false
/// when no Windows VM runs (no socket).
///
/// # Errors
/// The VM's QMP socket is there but does not answer.
fn windows_has_gpu(socket: &Path) -> Result<bool> {
    if !socket.try_exists()? {
        return Ok(false);
    }
    let ids = legionctl_core::qmp::Qmp::connect(socket)?.execute(
        "qom-list",
        Some(serde_json::json!({"path": "/machine/peripheral"})),
    )?;
    Ok(ids
        .as_array()
        .is_some_and(|ids| ids.iter().any(|i| i["name"] == "gpu0")))
}

/// Whether user unit `unit` is active.
/// The user unit `unit` is active (`systemctl --user is-active`: exit 0).
///
/// # Errors
/// systemctl does not run.
pub(crate) fn unit_active(unit: &str) -> Result<bool> {
    Ok(Command::new("systemctl")
        .args(["--user", "is-active", "-q", unit])
        .status()
        .context("running systemctl")?
        .success())
}

/// A `workspace.pack` job from its input (`repo`, `service`, `as`, `gpu`): at its start, and when a restart cut it off.
fn pack_job(
    input: &serde_json::Value,
    session: &legionctl_core::config::SessionConfig,
) -> Result<Lines> {
    let repo = Path::new(
        input["repo"]
            .as_str()
            .context("the job has no repository")?,
    );
    match serde_json::from_value(input["as"].clone())? {
        PackKind::Container => pack(repo, input["service"].as_str(), &session.container),
        PackKind::Vm => pack_vm(repo, &session.container, &session.vm, input["gpu"] == true),
    }
}

/// The CPU list of a core class: the P-cores, the E-cores or every online CPU.
fn core_cpus(class: Option<CoreClass>) -> Result<Option<String>> {
    let root = SysRoot::from_env();
    let cpus = match class {
        None => return Ok(None),
        Some(CoreClass::All) => root
            .read("/sys/devices/system/cpu/online")?
            .context("no /sys/devices/system/cpu/online")?,
        Some(class) => {
            let key = if class == CoreClass::P { "p" } else { "e" };
            legionctl_machine::gpu::core_class_cpus(&root, key)?.with_context(|| {
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

/// The machine, its profile, the Fn+Q ledger and the configset layers, as the planners need them.
struct Plan {
    root: SysRoot,
    profile: Profile,
    ledger: Ledger,
    layers: Vec<Layer>,
    /// power-profiles-daemon's profile, read once with the plan.
    ppd: Option<String>,
}

impl Plan {
    fn load(profile: Option<&Path>) -> Result<Plan> {
        Plan::at(SysRoot::from_env(), profile)
    }

    /// The machine under `root`; without `profile`, the profile installed there.
    fn at(root: SysRoot, profile: Option<&Path>) -> Result<Plan> {
        let installed = root.path(profile::DEFAULT_PATH);
        let ledger = Ledger::read(&root)?;
        let profile = profile::load(Some(profile.unwrap_or(&installed)))?.0;
        let layers = configset::load(&root, &profile, &ledger)?;
        let ppd = legionctl_machine::ppd::active(&root)?;
        Ok(Plan {
            root,
            profile,
            ledger,
            layers,
            ppd,
        })
    }

    /// What the daemon would write now (`power apply`, auto), what `doctor` finds and the knobs.
    fn decisions(&self, dir: &Path, events: Events) -> Result<Recording> {
        let machine = self.machine();
        Ok(Recording {
            dir: dir.display().to_string(),
            paths: 0,
            plan: dry_run(&machine.apply(None, Options::default())?.1).lines,
            findings: legionctl_machine::doctor::doctor(&machine)
                .into_iter()
                .map(|w| w.text)
                .collect(),
            knobs: knobs::catalog(&machine)?.into_wire(),
            events,
        })
    }

    fn machine(&self) -> Machine<'_> {
        Machine {
            root: &self.root,
            profile: &self.profile,
            ledger: &self.ledger,
            ppd_current: self.ppd.clone(),
            has_command: &has_command,
            layers: &self.layers,
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
    /// Mutter's monitor state, from the desktop session.
    fn mutter_state(&self) -> Result<crate::display::State> {
        Ok(self.runtime.block_on(async {
            let session = zbus::Connection::session().await?;
            crate::display::DisplayConfigProxy::new(&session)
                .await?
                .get_current_state()
                .await
        })?)
    }

    /// The active monitors from Mutter, and the fonts' antialiasing.
    fn monitors(&self) -> Result<(Vec<crate::display::Shown>, String)> {
        let state = self.mutter_state()?;
        let antialiasing = gsettings(&["get", INTERFACE, "font-antialiasing"])?
            .trim_matches('\'')
            .to_string();
        Ok((
            crate::display::shown(&SysRoot::from_env(), &state)?,
            antialiasing,
        ))
    }

    /// The graphics findings, and with a desktop session (Mutter on the session bus) the monitors' findings.
    fn display_findings(&self) -> Result<Vec<Finding>> {
        let (root, home) = (SysRoot::from_env(), home());
        let mut out = legionctl_machine::graphics::findings(&root, &home)?;
        match self.monitors() {
            Ok((monitors, antialiasing)) => out.extend(crate::display::findings(
                &root,
                &home,
                &monitors,
                &antialiasing,
            )?),
            // no desktop session: no session bus, or no Mutter on it
            Err(e) if no_desktop(&e) => {}
            Err(e) => out.push(Finding::system(
                "monitors",
                format!("the monitors cannot be read from Mutter: {e:#}"),
            )),
        }
        Ok(out)
    }

    /// The value that `fix` changes, as the change that gives it back.
    fn current(&self, fix: &Fix) -> Result<Fix> {
        Ok(match fix {
            Fix::File { path, .. } => Fix::File {
                path: path.clone(),
                content: match std::fs::read(path) {
                    Ok(bytes) => Some(String::from_utf8(bytes).with_context(|| {
                        format!("{} is not text; it stays as it is", path.display())
                    })?),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
                },
            },
            Fix::Setting { key, .. } => Fix::Setting {
                key: key.clone(),
                value: gsettings(&["get", INTERFACE, key])?,
            },
            Fix::Mode { connector, .. } => Fix::Mode {
                connector: connector.clone(),
                mode: crate::display::mode_of(&self.mutter_state()?, connector)
                    .with_context(|| format!("{connector} is not active"))?,
            },
        })
    }

    fn apply(&self, fix: &Fix, method: u32) -> Result<()> {
        match fix {
            Fix::File {
                path,
                content: Some(content),
            } => {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(path, content)
                    .with_context(|| format!("writing {}", path.display()))?;
            }
            Fix::File {
                path,
                content: None,
            } => std::fs::remove_file(path)
                .with_context(|| format!("removing {}", path.display()))?,
            Fix::Setting { key, value } => {
                gsettings(&["set", INTERFACE, key, value])?;
            }
            Fix::Mode { connector, mode } => self.runtime.block_on(async {
                let session = zbus::Connection::session().await?;
                let mutter = crate::display::DisplayConfigProxy::new(&session).await?;
                let state = mutter.get_current_state().await?;
                crate::display::apply_mode(&mutter, &state, connector, mode, method).await
            })?,
        }
        Ok(())
    }

    /// Make the user-level changes `fixes` (Mutter's `method` for a monitor's mode): each line names the new value
    /// and the old one. The event log keeps the old values first (an event `display.change`), so that `undo` gives
    /// them back even when a change fails half way.
    fn change(&self, ctx: &Ctx, fixes: &[Fix], method: u32, dry: bool) -> Result<Changes> {
        let mut was = fixes
            .iter()
            .map(|f| self.current(f))
            .collect::<Result<Vec<_>>>()?;
        let mut lines: Vec<String> = fixes
            .iter()
            .zip(&was)
            .map(|(new, old)| format!("{}: {} (was {})", target(new), value(new), value(old)))
            .collect();
        if dry || fixes.is_empty() {
            return Ok(Changes {
                lines,
                dry_run: dry,
            });
        }
        was.reverse();
        let event = Db::open_default()?.record(
            &ctx.actor,
            "display.change",
            &serde_json::json!({"undo": was, "method": method}),
        )?;
        for fix in fixes {
            self.apply(fix, method)?;
        }
        lines.push(format!("legionctl undo {event} gives the old values back"));
        Ok(Changes {
            lines,
            dry_run: false,
        })
    }

    /// A boot action: the daemon's plan for a dry run (root reads what the user cannot, todo-128), else the daemon
    /// carries it out.
    fn boot_action(&self, action: &'static str, dry: bool) -> Result<Changes> {
        let lines = if dry {
            self.system(async move |p| p.plan("boot", action).await)?
        } else {
            self.system(async move |p| p.boot(action).await)?
        };
        Ok(Changes {
            lines,
            dry_run: dry,
        })
    }

    /// A greeter action: the daemon's plan (it refuses first, before the polkit dialog), then for a run the daemon
    /// carries it out.
    fn greeter_action(&self, action: &'static str, dry: bool) -> Result<Changes> {
        let plan = self.system(async move |p| p.plan("greeter", action).await)?;
        if dry {
            return Ok(Changes {
                lines: plan,
                dry_run: true,
            });
        }
        Ok(Changes {
            lines: self.system(async move |p| p.greeter(action).await)?,
            dry_run: false,
        })
    }

    /// A kernel action on `name` (with `line` for `cmdline`): its plan for a dry run, else the daemon carries it out
    /// (the plan refuses first, before the polkit dialog).
    fn kernel_action(
        &self,
        action: &'static str,
        name: String,
        line: String,
        dry: bool,
        plan: impl FnOnce(
            &SysRoot,
            &legionctl_machine::boot::BootState,
        ) -> Result<Vec<legionctl_machine::policy::Step>>,
    ) -> Result<Changes> {
        let root = SysRoot::from_env();
        let steps = plan(&root, &legionctl_machine::boot::state(&root)?)?;
        if dry {
            return Ok(dry_run(&steps));
        }
        Ok(Changes {
            lines: self.system(async move |p| p.kernel(action, &name, &line).await)?,
            dry_run: false,
        })
    }

    /// A machine change through `legionctl-system`; polkit asks the human.
    pub(crate) fn system<T>(
        &self,
        call: impl AsyncFnOnce(&System1Proxy<'_>) -> zbus::Result<T>,
    ) -> Result<T> {
        self.runtime.block_on(async {
            let conn = zbus::Connection::system()
                .await
                .context("legionctl-system does not answer: connecting to the system bus")?;
            let proxy = System1Proxy::new(&conn).await?;
            call(&proxy).await.map_err(|e| match &e {
                zbus::Error::FDO(fdo) => match **fdo {
                    zbus::fdo::Error::AccessDenied(_) => {
                        coded(Code::Denied, format!("legionctl-system: {e}"))
                    }
                    zbus::fdo::Error::InvalidArgs(_) => {
                        coded(Code::InvalidInput, format!("legionctl-system: {e}"))
                    }
                    _ => coded(Code::Failed, format!("legionctl-system: {e}")),
                },
                // the bus has no daemon: it is off, or its start failed
                zbus::Error::MethodError(name, ..)
                    if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" =>
                {
                    coded(
                        Code::Unavailable,
                        format!("legionctl-system is off; {}", setup::SYSTEM_ON),
                    )
                }
                _ => coded(Code::Failed, format!("legionctl-system: {e}")),
            })
        })
    }

    fn hook_table(config: &Resolved) -> HookTable {
        HookTable {
            actions: hooks::actions(config).into_wire(),
            bindings: hooks::bindings(config).into_wire(),
        }
    }
}

/// The events that a feedback's state.json keeps: the newest.
const FEEDBACK_EVENTS: u32 = 50;

pub(crate) fn dossiers() -> Result<Dossiers> {
    Dossiers::open(&config(None)?.config.dossier)
}

/// Where the repository commands of a call run (O-1): in the caller's own session sandbox, or on the host for the
/// user, who calls them.
fn jail(
    ctx: &Ctx,
    store: &Dossiers,
    project: Option<&ClientPath>,
) -> Result<Option<legionctl_core::session::Jail>> {
    match &ctx.principal {
        Principal::Session(id) => Ok(Some(store.jail(id, &config(project)?.config)?)),
        Principal::User => Ok(None),
    }
}

/// The throttle reasons logged since boot, as `legionctl-system` reads them (the MSR is root's); `Err`: why they are
/// unknown. Doctor says more about a daemon that does not answer.
fn throttle_logged(op: &Operator) -> std::result::Result<Vec<String>, String> {
    match op.system(async |p| p.sample().await) {
        Ok(json) => serde_json::from_str::<RootSample>(&json)
            .map_err(|e| format!("legionctl-system sent a sample of another build: {e}"))?
            .throttle_logged
            .ok_or_else(|| "load the msr module: there is no /dev/cpu/0/msr".into()),
        Err(e) if code_of(&e) == Code::Unavailable => Err("legionctl-system is off".into()),
        Err(_) => Err("legionctl-system does not answer".into()),
    }
}

/// Doctor's findings of the daemon's sample: CPU throttling of note logged since boot, each eBPF sensor that does not
/// run, or why there is no sample.
fn sample_findings(op: &Operator) -> Result<Vec<legionctl_api::Finding>> {
    Ok(match op.system(async |p| p.sample().await) {
        Ok(json) => {
            let sample = serde_json::from_str::<RootSample>(&json)?;
            sample
                .throttle_logged
                .filter(|r| {
                    r.iter()
                        .any(|r| ["PROCHOT", "critical temperature"].contains(&r.as_str()))
                })
                .map(|r| state(format!("CPU throttling logged: {}", r.join(", "))))
                .into_iter()
                .chain(sample.sensor_errors.into_iter().map(|(sensor, e)| {
                    state(format!("the eBPF {sensor} sensor does not run: {e}"))
                }))
                .collect()
        }
        // an off daemon is the setup finding of legionctl-system, with its Turn on
        Err(e) if code_of(&e) == Code::Unavailable => Vec::new(),
        Err(e) => vec![state(format!(
            "{e:#} (fans, throttle reasons and the eBPF sensor are unknown)"
        ))],
    })
}

/// The agent setup gaps of `doctor`, each with the action that installs the setup.
fn setup_findings() -> Result<Vec<legionctl_api::Finding>> {
    let report = setup::check(
        &Sources::resolve(&Dirs::from_env(), None)?,
        &config(None)?.config.session,
    )?;
    report
        .needs_attention()
        .iter()
        .map(|i| {
            let text = format!("setup: {} for {} is {}", i.what, i.agent, i.status);
            // no setup step mends an extension that GNOME Shell refused
            if i.status == legionctl_machine::system::OSD_BROKEN {
                return Ok(state(text));
            }
            Ok(legionctl_api::Finding {
                text,
                // ctx comes from its own installer, only when the user asks for it
                // a service gets its own step only: a missing ctx does not stop it
                actions: vec![if matches!(i.agent.as_str(), "machine" | "user") {
                    crate::resources::action(
                        "Turn on",
                        "setup.turn-on",
                        serde_json::json!({ "service": i.what }),
                    )?
                } else {
                    crate::resources::action(
                        if i.what == "ctx" {
                            "Install ctx"
                        } else {
                            "Install the agent setup"
                        },
                        "setup.apply",
                        serde_json::json!({ "install_ctx": i.what == "ctx" }),
                    )?
                }],
            })
        })
        .collect()
}

/// A finding that legionctl cannot change: its text only.
fn state(text: String) -> legionctl_api::Finding {
    legionctl_api::Finding {
        text,
        actions: Vec::new(),
    }
}

/// A check of `doctor` that could not run: why, and that the next `doctor` tries again.
fn not_run(name: &str, e: &anyhow::Error) -> legionctl_api::Finding {
    state(format!(
        "the {name} check could not run ({e:#}); the next health check runs it again"
    ))
}

/// The agent side of `doctor`: a Claude Code plugin of another build, stale dossier bindings, workspaces that drifted
/// from their record.
fn agent_findings() -> Result<Vec<String>> {
    let mut w = legionctl_core::plugin::findings(&home());
    if legionctl_core::migrate::pending() {
        w.push(legionctl_core::migrate::state(&Db::open_default()?));
        return Ok(w);
    }
    let store = dossiers()?;
    w.extend(store.sync_registry()?);
    for lock in store.locks()?.iter().filter(|l| l.stale) {
        w.push(format!(
            "dossier {} is bound to session {}, idle past dossier.stale_hours; the next session that uses it takes it over",
            lock.dossier, lock.session
        ));
    }
    // the checks of a repository once, from a dossier's own workspace: not again for each topic worktree and dossier
    // (EV-review-11 U-15: 19-22 equal lines)
    let mut checked = std::collections::HashSet::new();
    for ws in store.workspaces(None)? {
        // the link of the home (design 23, E1a), where the main checkout and the workspace have one
        for dir in [&ws.repo, &ws.path] {
            w.extend(legionctl_core::home::finding(&store, dir));
        }
        // a missing worktree is the workspace's drift below
        if !ws.is_folder()
            && ws.topic.is_empty()
            && ws.path.is_dir()
            && checked.insert(ws.repo.clone())
        {
            w.extend(store.check_findings(&ws.path));
        }
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
    // workspaces of one repository give its findings once
    let mut seen = std::collections::BTreeSet::new();
    w.retain(|f| seen.insert(f.clone()));
    Ok(w)
}

/// The operator's JOURNAL entry before a compaction of `session`: what it recorded of the session (status changes of
/// plan rows and artifacts, checkpoints), as links. Immutable (class JOURNAL).
fn compaction_journal(
    store: &Dossiers,
    dossier: &str,
    session: &str,
    trigger: &str,
    health: &str,
) -> Result<String> {
    let mut lines = vec![
        "---".to_string(),
        "class: JOURNAL".into(),
        "status: recorded".into(),
        "---".into(),
        format!("# Compaction ({trigger}) of session {session}"),
        String::new(),
        format!(
            "The operator's record of this session before the compaction; the session's own entry follows it. \
             Health: {}.",
            health
                .trim_start_matches(&format!("{dossier}/"))
                .trim_end_matches(".md")
        ),
        String::new(),
        "## Status changes (node: from -> to)".into(),
    ];
    let changes = store.status_changes_of(dossier, session)?;
    lines.extend(changes.iter().map(|c| {
        format!(
            "- {}: {} -> {}",
            c.node,
            c.from.as_deref().unwrap_or("new"),
            c.to.as_deref().unwrap_or("none")
        )
    }));
    if changes.is_empty() {
        lines.push("- none".into());
    }
    lines.push(String::new());
    lines.push("## Checkpoints".into());
    let (commits, _) = Db::open_default()?.events_page(
        &legionctl_core::db::EventFilter {
            kinds: &[legionctl_core::codegraph::COMMIT_EVENT],
            field: Some(("$.session", session)),
            naming: None,
        },
        0,
        200,
    )?;
    lines.extend(
        commits
            .iter()
            .filter_map(|e| e.payload["commit"].as_str())
            .map(|c| format!("- {c}")),
    );
    if commits.is_empty() {
        lines.push("- none".into());
    }
    Ok(lines.join("\n") + "\n")
}

/// The line of the session start after a compaction: read the plan and the newest JOURNAL and HEALTH entries first,
/// then write the session's own entry, every item with its source (todo-156).
fn after_compaction(store: &Dossiers, dossier: &str) -> String {
    let newest = |class: &str| {
        store.newest(dossier, class).map_or_else(
            || format!("(no {class} entry)"),
            |f| format!("{dossier}/{f}"),
        )
    };
    format!(
        "legionctl compaction: the details live in the dossier. Read {dossier}/plan.md, {} and {} first. Then write \
         your own entry (legionctl dossier add JOURNAL <slug>): state, decisions and open work as links (plan#todo-NN, \
         MEMORY-n, DESIGN-n, EVIDENCE/..., JOURNAL/NN), never copies; each item names its source. New memories go as \
         an index plus one file per work area, each linked. JOURNAL and HEALTH entries are immutable: a correction is \
         a new entry that supersedes one.",
        newest("JOURNAL"),
        newest("HEALTH")
    )
}

/// What a delegate of a session with dossier `d` reads first (user rule 2026-10-16, MEMORY-14; skill
/// sessions-and-agents rule 25).
fn delegation_rule(d: &str) -> String {
    format!(
        "legionctl delegation rule (dossier {d}): read the full dossier plan (`legionctl dossier read {d}/plan.md`) \
         and the MEMORY and DESIGN files its rows name yourself; do not rely only on this prompt. If you review or \
         audit: be adversarial and thorough, and report what you did not check. Details live in the dossier; keep \
         your messages and your final report short and objective, and point to dossier artifacts (plan#todo-NN, \
         MEMORY-n, EVIDENCE/..., commits) instead of pasting details."
    )
}

/// `prompt` of thread `id` for an agent with no session of the thread in `dir` yet (its first run, or a run after the
/// thread moved to a repository): the conversation so far comes first, so it goes on there.
fn conversation_prompt(store: &Dossiers, id: i64, dir: &Path, prompt: &str) -> Result<String> {
    let earlier = store.thread_conversation(id, 12)?;
    if earlier.is_empty() {
        return Ok(prompt.to_string());
    }
    let lines: Vec<String> = earlier
        .iter()
        .map(|(who, text)| format!("{who}: {}", text.chars().take(2000).collect::<String>()))
        .collect();
    Ok(format!(
        "The conversation of this thread so far (it now works in {}):\n{}\n\nThe user's new message:\n{prompt}",
        dir.display(),
        lines.join("\n")
    ))
}

/// The user's dossier (no session: the app, a terminal): the fact `app` `dossier` of the user, global.
pub(crate) fn user_dossier() -> Result<Option<String>> {
    let facts = crate::facts::at_op(&legionctl_api::FactsAtIn {
        entity: Some("app".into()),
        repo: ClientPath(PathBuf::from("/")),
        dossier: None,
        thread: None,
        time: None,
        recorded: None,
        rev: None,
        winners: true,
    })?;
    Ok(facts
        .into_iter()
        .find(|f| f.attribute == "dossier")
        .and_then(|f| f.value.as_str().map(String::from)))
}

/// Choose the user's dossier (`None`: no dossier).
fn user_dossier_set(ctx: &Ctx, repo: &ClientPath, dossier: Option<&str>) -> Result<()> {
    crate::facts::set(
        ctx,
        &legionctl_api::FactSetIn {
            entity: "app".into(),
            attribute: "dossier".into(),
            value: serde_json::json!(dossier).to_string(),
            scope: legionctl_api::FactScope::Global,
            repo: repo.clone(),
            dossier: None,
            thread: None,
            valid_from: None,
            valid_to: None,
            from_ref: None,
            to_ref: None,
            anchor: None,
            evidence: Some("dossier use".into()),
            confidence: 1.0,
            corrects: None,
        },
    )?;
    Ok(())
}

/// The agent session of an operation that only an agent does (it writes the dossier that its session holds).
pub(crate) fn session(id: &SessionId) -> Result<&str> {
    anyhow::ensure!(
        !id.0.is_empty(),
        "only an agent does this, in its session: ask the agent that works in the dossier"
    );
    Ok(&id.0)
}

/// The dossier of a session that the user starts in `dir` with no dossier named: the one last used in its
/// repository, else the dossier the user chose.
fn work_dossier(store: &Dossiers, dir: &Path) -> Result<String> {
    match store.linked(dir)? {
        Some((_, last)) => Ok(last),
        None => user_dossier()?.with_context(|| {
            format!(
                "no dossier was used in {} and none is chosen: give --dossier <name>, or choose one with \
                 `legionctl dossier use <name>` (legionctl dossier list)",
                dir.display()
            )
        }),
    }
}

/// The caller's dossier: an agent session's binding, else the dossier the user chose (`dossier use`). One source for
/// every operation that acts on "my dossier" (DESIGN-15).
pub(crate) fn dossier_of(id: &SessionId) -> Result<String> {
    if id.0.is_empty() {
        return user_dossier()?.context(
            "no dossier is chosen: choose one with `legionctl dossier use <name>` (legionctl dossier list)",
        );
    }
    dossiers()?
        .active(&id.0)?
        .context("this session has no dossier: bind one with dossier use <name>")
}

/// The linked repository of a session with no dossier, and at the start and at each prompt the binding of its
/// dossier (so a dossier that another session held is taken once it is free).
type Linked = (Option<(PathBuf, String)>, Option<Result<String, String>>);
fn bind_linked(
    store: &Dossiers,
    resolved: &Resolved,
    input: &HookRunIn,
    active: &mut Option<String>,
) -> Result<Linked> {
    let session = input.session.as_ref().filter(|s| !s.0.is_empty());
    let linked = match (session, &active) {
        (Some(_), None) => store.linked(input.project.get()?)?,
        _ => None,
    };
    let rebound = match (session, &linked) {
        (Some(s), Some(l))
            if matches!(
                Event::from(input.event),
                Event::SessionStart | Event::Prompt
            ) && resolved.config.dossier.auto_bind =>
        {
            Some(rebind(store, &s.0, l, active))
        }
        _ => None,
    };
    Ok((linked, rebound))
}

/// A session in a linked repository takes the dossier last used there, so nobody has to bind it again. Returns the
/// line for the agent and sets `active`, or gives why the binding failed.
fn rebind(
    store: &Dossiers,
    session: &str,
    (place, last): &(PathBuf, String),
    active: &mut Option<String>,
) -> Result<String, String> {
    match store.bind(session, "hook", last, false, Some(place)) {
        Ok(_) => {
            *active = Some(last.clone());
            Ok(format!(
                "legionctl dossier: {last} (bound by itself: the last dossier used in {}; read its plan.md and newest \
                 MEMORY, and use another with dossier_use if this work needs one)",
                place.display()
            ))
        }
        Err(e) => Err(format!("{e:#}")),
    }
}

/// Why this hook call stops the prompt (the gate, for the user) or the end of the reply (for the agent), if it does.
fn blocking_reason(
    resolved: &Resolved,
    input: &HookRunIn,
    (linked, rebound): (Option<&(PathBuf, String)>, Option<&str>),
    active: Option<&str>,
    place: Option<&Place>,
) -> Result<Option<String>> {
    let config = &resolved.config;
    // the gate and the nudge hold only in a repository linked to legionctl work; elsewhere legionctl stays out
    let unbound = linked.filter(|_| active.is_none());
    let runs_dossier = input
        .prompt
        .as_deref()
        .is_some_and(|p| p.contains("legionctl:dossier"));
    if let Some((repo, last)) = unbound
        && Event::from(input.event) == Event::Prompt
        && config.dossier.gate
        && !runs_dossier
    {
        return gate_message(resolved, repo, last, rebound).map(Some);
    }
    if Event::from(input.event) != Event::Stop {
        return Ok(None);
    }
    // before the reply ends, the agent (not the user) hears what is not set up, and sets it up first
    if config.dossier.nudge.contains(&Event::Stop) && unbound.is_some() {
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
        Event::Stop => store.turn(
            session,
            dossier,
            &legionctl_core::workspace::work_state(store, ws)?,
        )?,
        Event::Prompt => store.nudge_state(session, dossier)?,
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
        store.told(session, dossier, level)?;
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
    (session, compacted): (&str, bool),
    active: Option<&str>,
    (linked, rebound): (bool, Option<Result<String, String>>),
    place: Option<&Place>,
) -> Vec<String> {
    let nudge = linked && resolved.config.dossier.nudge.contains(&Event::SessionStart);
    let mut lines = vec![match (active, rebound) {
        (Some(_), Some(Ok(line))) => line,
        (Some(d), _) => format!("legionctl dossier: {d} (session {session})"),
        (None, Some(Err(why))) => {
            let failed = format!("legionctl: the dossier of this repository cannot be bound: {why}");
            if nudge { format!("{failed}\n{}", nudge_line(resolved)) } else { failed }
        }
        (None, _) if nudge => nudge_line(resolved),
        (None, _) => {
            "legionctl dossier: none (no dossier was used in this folder; the user can choose one with \
             /legionctl:dossier)"
                .into()
        }
    }];
    if let (true, Some(d)) = (compacted, active) {
        lines.push(after_compaction(store, d));
    }
    match place.map(|p| (p, &p.workspace)) {
        Some((_, Some(w))) => {
            lines.push(format!(
                "legionctl workspace: {} on {} (base {}); make code changes there",
                w.path.display(),
                w.branch,
                w.base
            ));
            match store.workspace_status(w) {
                Ok(s) => lines.extend(
                    s.drift
                        .into_iter()
                        .map(|d| format!("legionctl workspace drift: {d}")),
                ),
                Err(e) => lines.push(format!("legionctl workspace drift unknown: {e:#}")),
            }
        }
        Some((p, None)) if resolved.config.workspace.nudge => lines.push(format!(
            "legionctl: no workspace for {} yet. Before code changes, open one with the legionctl MCP tool \
             workspace_open and work in its path.",
            p.repo.display()
        )),
        _ => {}
    }
    // the checks of the branch where the agent works (its workspace), else of the main checkout
    if let Some(dir) = place.map(|p| p.workspace.as_ref().map_or(&p.repo, |w| &w.path))
        && resolved.config.checks.nudge
    {
        match legionctl_core::checks::missing_kinds(dir) {
            Ok(missing) if missing.is_empty() => {}
            Ok(missing) => lines.push(format!(
                "legionctl: {}.",
                legionctl_core::checks::missing_line(&missing)
            )),
            Err(e) => lines.push(format!(
                "legionctl: the checks of {} do not resolve: {e:#}",
                dir.display()
            )),
        }
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

/// What a blocked prompt shows the user: the repository's dossier, why this session has none, the command that passes
/// the gate and the most recent dossiers.
fn gate_message(
    resolved: &Resolved,
    repo: &Path,
    last: &str,
    failed: Option<&str>,
) -> Result<String> {
    let recent: Vec<String> = Dossiers::open(&resolved.config.dossier)?
        .list()?
        .into_iter()
        .take(5)
        .map(|d| d.name)
        .collect();
    Ok(format!(
        "legionctl: {} works in dossier {last}, but this session has no dossier{}, so the prompt was not sent. Choose \
         one with `/legionctl:dossier use <name>`, then send the prompt again. Recent dossiers: {}.",
        repo.display(),
        failed
            .map(|f| format!(" (it could not take {last}: {f})"))
            .unwrap_or_default(),
        recent.join(", ")
    ))
}

/// The sets that a session (the main agent) requests: those of the config main-agent, and the start's flags.
fn main_agent_sets(config: &legionctl_core::config::Config, flags: &[String]) -> Vec<String> {
    config
        .agents
        .configs
        .get("main-agent")
        // no config main-agent: it requests nothing; its harness's and languages' sets still come
        .map_or_else(Vec::new, |c| c.capabilities.clone())
        .into_iter()
        .chain(flags.iter().cloned())
        .collect()
}

/// The config for work in `project`; a repository layer file with keys that wait for the user's first answer gets
/// its question (O-2), and resolves without them until then.
pub(crate) fn config(project: Option<&ClientPath>) -> Result<Resolved> {
    let resolved = resolve(&Dirs::from_env(), path(project)?)?;
    crate::trust::ask(&resolved.unapproved);
    Ok(resolved)
}

impl Operator {
    /// The rows of `legionctl sensors`: a new sample with the ring's history, and the daemon's fan state.
    fn readings(&self) -> Result<Vec<sensors::Reading>> {
        let earlier: Vec<telemetry::Sample> = self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .collect();
        let now = self.sample(std::time::Duration::from_millis(250))?;
        // a daemon that does not answer is the fan rows' state, with the reason
        let fan = self
            .system(async |p| p.fan_status().await)
            .and_then(|json| Ok(serde_json::from_str::<fan::FanState>(&json)?))
            .map_err(|e| format!("{e:#}"));
        let (profile, _) = profile::load(None)?;
        sensors::view(
            &SysRoot::from_env(),
            &now,
            &earlier,
            fan.as_ref().map_err(String::as_str),
            &profile.paths.ac_adapter,
        )
    }

    /// The vitals overlay's lines (`overlay`).
    pub fn vitals(&self) -> Result<Vec<String>> {
        Ok(sensors::vitals(&self.readings()?))
    }

    /// One telemetry sample over `window`; the root-only part comes from `legionctl-system`, and stays unknown when
    /// the daemon does not answer.
    ///
    /// # Errors
    /// A machine file cannot be read.
    pub fn sample(&self, window: std::time::Duration) -> Result<telemetry::Sample> {
        let root_part = || {
            self.runtime
                .block_on(async {
                    let conn = zbus::Connection::system().await?;
                    let json = System1Proxy::new(&conn).await?.sample().await?;
                    anyhow::Ok(serde_json::from_str::<RootSample>(&json)?)
                })
                .unwrap_or_default()
        };
        let sample = telemetry::sample(&SysRoot::from_env(), window, &root_part)?;
        let mut ring = self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if ring.len() == RING {
            ring.pop_front();
        }
        ring.push_back(sample.clone());
        Ok(sample)
    }
}

impl Operator {
    /// Resume the jobs in `state`: `running` ones a restart cut off (at most 3 attempts), `offloaded` ones when
    /// memory eased. Each goes on in the background: a build runs again (a kernel build from its last step); a kernel
    /// bench series takes its next step on the kernel that started; a headless run goes on in its session (same
    /// dossier and workspace, and its ACP session when the agent can load it) with the task and a note that it was cut
    /// off.
    pub fn resume_jobs(self: &std::sync::Arc<Self>, state: &str) -> Result<()> {
        let db = Db::open_default()?;
        for job in db.jobs_in(state)? {
            // a member's wake goes on as its next wake (`thread::init`)
            if job.kind == crate::thread::WAKE {
                continue;
            }
            if state == "running" && job.attempts >= 3 {
                db.job_end(job.id, &Err("cut off by a restart 3 times".into()))?;
                continue;
            }
            db.job_again(job.id)?;
            let operator = self.clone();
            std::thread::spawn(move || {
                let db = match Db::open_default() {
                    Ok(db) => db,
                    Err(e) => return crate::faults::failed("jobs", crate::JOBS, &e),
                };
                let outcome = match job.kind.as_str() {
                    "session.run" => {
                        let out = operator.continue_run(job.id, &job.input);
                        STOPS
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(&job.id);
                        end_job(&db, job.id, &out)
                    }
                    "images.build" => {
                        let repo = PathBuf::from(job.input["repo"].as_str().unwrap_or_default());
                        end_job(&db, job.id, &build_images(&repo))
                    }
                    crate::kernels::SERIES => operator.series_step(&db, &job),
                    crate::kernels::BUILD => {
                        let out = serde_json::from_value(job.input.clone())
                            .map_err(anyhow::Error::from)
                            .and_then(|build| operator.run_build(job.id, &build));
                        end_job(&db, job.id, &out)
                    }
                    "workspace.pack" => {
                        let repo = PathBuf::from(job.input["repo"].as_str().unwrap_or_default());
                        let out = config(Some(&ClientPath(repo)))
                            .and_then(|r| pack_job(&job.input, &r.config.session));
                        end_job(&db, job.id, &out)
                    }
                    // never on its own (decided 2026-10-15): the user runs it again
                    legionctl_core::migrate::JOB => db.job_end(
                        job.id,
                        &Err(
                            "cut off by an operator restart: run legionctl dossier migrate again"
                                .into(),
                        ),
                    ),
                    crate::code::REFRESH => crate::code::queue(job.id, job.input.clone())
                        .or_else(|e| end_job(&db, job.id, &Err::<Lines, _>(e))),
                    other => db.job_end(job.id, &Err(format!("no way to resume a {other} job"))),
                };
                if let Err(e) = outcome {
                    crate::faults::failed(
                        "jobs",
                        crate::JOBS,
                        &e.context(format!("job {}", job.id)),
                    );
                }
            });
        }
        Ok(())
    }

    /// A headless run that a restart cut off, on in its own session.
    fn continue_run(&self, job: i64, input: &serde_json::Value) -> Result<RunOutput> {
        let id = input["session"]
            .as_str()
            .context("the job has no session")?;
        let prompt = input["prompt"].as_str().unwrap_or_default();
        let store = dossiers()?;
        let record = store.session(id)?;
        let resolved = resolve(&Dirs::from_env(), Some(&place_of(&record.workdir)))?;
        let token = legionctl_api::random_token()?;
        // a job from before the binding has no tools: an unbound run, as it was
        let tools: Vec<String> =
            serde_json::from_value::<Option<Vec<String>>>(input["tools"].clone())
                .context("the job's tools are not a list of operations")?
                .unwrap_or_default();
        let launch = store.resume_session(id, &resolved.config, None, &token, (true, &tools))?;
        Db::open_default()?.issue_token(&launch.id, &token)?;
        let note = format!(
            "A restart of legionctl cut off your work on this task; your workspace and your dossier hold what you did. \
             Go on with the task:\n\n{prompt}"
        );
        self.runtime.block_on(crate::acp::run(
            launch,
            &note,
            (job, input),
            stoppable(job),
            &store,
        ))
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
        Ok(legionctl_machine::snapshot::Snapshot::read(
            &plan.root,
            &plan.profile,
            detail,
            &config(None)?.config.load,
            throttle_logged(self),
        )?
        .into_wire())
    }

    fn load(&self, _: &Ctx, _: Empty) -> Result<LoadState> {
        Ok(load::read_load(&SysRoot::from_env(), &config(None)?.config.load)?.into_wire())
    }

    fn doctor(&self, _: &Ctx, input: ProfileIn) -> Result<Findings> {
        // a machine that cannot be planned is a finding; the other checks still run
        let mut findings: Vec<legionctl_api::Finding> =
            match Plan::load(path(input.profile.as_ref())?) {
                Ok(plan) => legionctl_machine::doctor::doctor(&plan.machine())
                    .into_iter()
                    .map(|w| {
                        Ok(legionctl_api::Finding {
                            text: w.text,
                            actions: w
                                .fix
                                .map(|(label, op, input)| {
                                    crate::resources::action(label, op, input)
                                })
                                .transpose()?
                                .into_iter()
                                .collect(),
                        })
                    })
                    .collect::<Result<_>>()?,
                Err(e) => vec![not_run("machine", &e)],
            };
        findings.extend(sample_findings(self)?);
        if let Ok(theirs) = self.system(async |p| p.version().await)
            && theirs != legionctl_machine::interface_version()
        {
            findings.push(state(
                "the system service legionctl-system is of another build than this operator: restart the computer \
                 after an upgrade"
                    .into(),
            ));
        }
        match self.display_findings() {
            Ok(found) => {
                for f in found {
                    let actions = if f.fixes.is_empty() {
                        Vec::new()
                    } else {
                        vec![crate::resources::action(
                            "Fix",
                            "display.fix",
                            serde_json::json!({ "finding": f.id }),
                        )?]
                    };
                    findings.push(legionctl_api::Finding {
                        text: f.text,
                        actions,
                    });
                }
            }
            Err(e) => findings.push(not_run("graphics", &e)),
        }
        match setup_findings() {
            Ok(found) => findings.extend(found),
            Err(e) => findings.push(not_run("agent setup", &e)),
        }
        // nothing moves in silence: the user logs in to each old profile in clauth (user decision 2026-10-17)
        if let Some((folder, names)) = legionctl_core::accounts::old_profiles() {
            findings.push(legionctl_api::Finding {
                text: if names.is_empty() {
                    format!(
                        "{} is legionctl's old folder of account profiles, empty and not used: delete it",
                        folder.display()
                    )
                } else {
                    format!(
                        "{} holds legionctl's old account profiles {}, not used: clauth keeps the logins now. Log in \
                         to each of them in clauth, then delete the folder",
                        folder.display(),
                        names.join(", ")
                    )
                },
                actions: names
                    .iter()
                    .map(|n| {
                        crate::resources::action(
                            &format!("Log in to {n}"),
                            "agents.accounts.login",
                            serde_json::json!({ "name": n }),
                        )
                    })
                    .collect::<Result<_>>()?,
            });
        }
        for (name, found) in [
            ("agents and workspaces", agent_findings()),
            ("operator fault", crate::faults::findings()),
        ] {
            match found {
                Ok(found) => findings.extend(found.into_iter().map(state)),
                Err(e) => findings.push(not_run(name, &e)),
            }
        }
        Ok(Findings { findings })
    }

    fn capabilities(&self, _: &Ctx, input: ProfileIn) -> Result<CapabilityList> {
        let (profile, _) = profile::load(path(input.profile.as_ref())?)?;
        Ok(CapabilityList {
            profile_id: profile.id,
            capabilities: profile.capabilities.into_wire(),
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
        let outputs = gpu::outputs(&root)?;
        Ok(GpuState {
            mode: gpu::mode(&outputs).into_wire(),
            desktop_gpu: gpu::desktop_preference(&root)?.into_wire(),
            outputs: outputs.into_wire(),
            nvidia_power: gpu::nvidia_power(&root)?.into_wire(),
        })
    }

    fn gpu_power(&self, _: &Ctx, input: GpuPowerIn) -> Result<Changes> {
        let sleep = input.state == OnOff::Off;
        // planned here too, so a refusal (a program uses the GPU) comes before the polkit dialog
        let plan = Plan::load(None)?;
        let steps = gpu::set_nvidia_power(&plan.machine(), sleep)
            .map_err(|e| coded(Code::InvalidInput, format!("{e:#}")))?;
        if input.dry_run {
            return Ok(dry_run(&steps));
        }
        Ok(Changes {
            lines: self.system(async move |p| p.set_gpu_power(sleep).await)?,
            dry_run: false,
        })
    }

    fn firmware_status(&self, _: &Ctx, _: Empty) -> Result<FirmwareDevices> {
        Ok(match fwupd::devices()? {
            Some(devices) => FirmwareDevices {
                devices: devices.into_wire(),
                missing: None,
            },
            None => FirmwareDevices {
                devices: Vec::new(),
                missing: Some(fwupd::NOT_INSTALLED.into()),
            },
        })
    }

    fn firmware_update(&self, ctx: &Ctx, input: FirmwareUpdateIn) -> Result<Changes> {
        let (device, argv) = fwupd::update_plan(&input.device)
            .map_err(|e| coded(Code::InvalidInput, format!("{e:#}")))?;
        let from = device.version.clone().unwrap_or_default();
        let to = device.update.clone().unwrap_or_default();
        let what = format!("{}: {from} -> {to}", device.name);
        if input.dry_run {
            return Ok(Changes {
                lines: vec![
                    format!("would update {what}"),
                    format!("would run: {}", argv.join(" ")),
                ],
                dry_run: true,
            });
        }
        let out = Command::new(&argv[0]).args(&argv[1..]).output()?;
        let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(String::from)
            .collect();
        if !out.status.success() {
            return Err(anyhow::anyhow!(
                "fwupdmgr update failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        // only a done update is a change
        let event = Db::open_default()?.record(
            &ctx.actor,
            "firmware.change",
            &serde_json::json!({"device": device.id, "name": device.name, "from": from, "to": to}),
        )?;
        lines.push(format!(
            "updated {what} (event {event}; fwupd keeps the old version in fwupdmgr get-history)"
        ));
        Ok(Changes {
            lines,
            dry_run: false,
        })
    }

    fn rgb_status(&self, _: &Ctx, _: Empty) -> Result<RgbState> {
        let address = rgb::address();
        let keyboard_usb = rgb::keyboard_present(&SysRoot::from_env())?;
        let mut state = RgbState {
            address: address.clone(),
            keyboard_usb,
            protocol: None,
            controllers: Vec::new(),
            profiles: Vec::new(),
            problem: None,
        };
        let read = rgb::Client::connect(&address).and_then(|mut c| {
            state.protocol = Some(c.protocol);
            state.controllers = c.controllers()?;
            state.profiles = c.profiles()?;
            Ok(())
        });
        state.problem = match read {
            Err(e) => Some(format!("{e:#}")),
            Ok(()) if !keyboard_usb => Some("the keyboard is not on the USB bus".into()),
            // OpenRGB names this keyboard's controller "Lenovo Legion ..." (its LEGION_7GEN8 detector)
            Ok(()) if !state.controllers.iter().any(|c| c.contains("Legion")) => Some(
                "OpenRGB does not list the keyboard: install OpenRGB's udev rules (60-openrgb.rules) so that it may \
                 open the keyboard's hidraw device, then start it again"
                    .into(),
            ),
            Ok(()) => None,
        };
        Ok(state)
    }

    fn overlay(&self, _: &Ctx, input: OverlayIn) -> Result<Lines> {
        let on = input.state == OnOff::On;
        self.overlay.store(on, std::sync::atomic::Ordering::Relaxed);
        Ok(Lines {
            lines: if on {
                let mut lines = vec![
                    "vitals overlay: on (every 2 s, drawn by the legionctl-osd extension)".into(),
                ];
                lines.extend(self.vitals()?);
                lines
            } else {
                vec!["vitals overlay: off".into()]
            },
        })
    }

    fn gpu_give(&self, _: &Ctx, input: GpuGiveIn) -> Result<Lines> {
        anyhow::ensure!(
            input.gpu == GpuName::Nvidia,
            "the iGPU drives the Linux desktop and the laptop panel: it moves only as SR-IOV functions, which this \
             kernel's i915 does not offer yet (todo-28 3c)"
        );
        let (group, now) = nvidia_owner(&SysRoot::from_env())?;
        let qmp_socket = legionctl_core::session::windows_root().join("qmp.sock");
        let name = |o: GpuOwner| match o {
            GpuOwner::Linux => "linux",
            GpuOwner::Windows => "windows",
            GpuOwner::None => "none",
        };
        if now == input.to {
            return Ok(Lines {
                lines: vec![format!("nvidia: already {}", name(now))],
            });
        }
        if input.to == GpuOwner::Windows {
            // before anything moves: without a VM the GPU would end with no owner
            legionctl_core::qmp::Qmp::connect(&qmp_socket)
                .context("no Windows VM runs (legionctl windows vm start)")?;
        }
        let mut lines = Vec::new();
        if now == GpuOwner::Windows {
            // the eject request: Windows releases the GPU (an app that holds it keeps it); never forced
            let mut qmp = legionctl_core::qmp::Qmp::connect(&qmp_socket)?;
            qmp.execute("device_del", Some(serde_json::json!({"id": "gpu0"})))?;
            qmp.wait_event("DEVICE_DELETED", "gpu0", std::time::Duration::from_secs(30))
                .context("Windows did not release the GPU: close what uses it in Windows, then try again")?;
            lines.push("nvidia: Windows released it".into());
        }
        if now == GpuOwner::Linux {
            lines.extend(self.system(async |p| p.gpu_release().await)?);
        }
        match input.to {
            GpuOwner::Linux => lines.extend(self.system(async |p| p.gpu_return().await)?),
            GpuOwner::Windows => {
                let mut qmp = legionctl_core::qmp::Qmp::connect(&qmp_socket)
                    .context("no Windows VM runs (legionctl windows vm start)")?;
                // the other functions first: function 0 starts the hot-plug of the slot
                for device in group.iter().rev() {
                    let function = device.rsplit('.').next().unwrap_or("0");
                    let id = format!("gpu{function}");
                    let mut add = serde_json::json!({"driver": "vfio-pci", "host": device, "bus": "gpu-port", "id": id, "addr": format!("0.{function}")});
                    if function == "0" {
                        add["multifunction"] = "on".into();
                    }
                    qmp.execute("device_add", Some(add))?;
                }
                lines.push("nvidia: hot-plugged into Windows".into());
            }
            GpuOwner::None => {}
        }
        lines.push(format!("nvidia: {} -> {}", name(now), name(input.to)));
        Ok(Lines { lines })
    }

    fn vfio_status(&self, _: &Ctx, _: Empty) -> Result<VfioDevices> {
        let json = self.system(async move |p| p.passthroughs().await)?;
        let items: Vec<legionctl_machine::vfio::Candidate> = serde_json::from_str(&json)?;
        Ok(VfioDevices {
            items: items.into_wire(),
        })
    }

    fn vfio_bind(&self, _: &Ctx, input: VfioBindIn) -> Result<Changes> {
        let memlock = input.memlock_mib.unwrap_or(0) << 20;
        let pid = input.pid.unwrap_or(0);
        Ok(Changes {
            lines: self.system(async move |p| p.passthrough(&input.devices, pid, memlock).await)?,
            dry_run: false,
        })
    }

    fn vfio_unbind(&self, _: &Ctx, input: VfioIn) -> Result<Changes> {
        Ok(Changes {
            lines: self.system(async move |p| p.passthrough_end(&input.devices).await)?,
            dry_run: false,
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
                gpu::core_class_cpus(&root, class)?
                    .context("this CPU does not report performance/efficiency core groups")?,
            ),
        };
        Ok(RunEnv {
            env: gpu::render_env(&root, gpu)?
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
            lines: self.system(async move |p| p.apply_power(&name).await)?,
            dry_run: false,
        })
    }

    fn gpu_primary(&self, _: &Ctx, input: GpuPrimaryIn) -> Result<Changes> {
        let choice = input.choice;
        // checked here too, so an invalid choice does not ask for a password first
        if input.release_nvidia && gpu::DesktopGpu::from(choice) == gpu::DesktopGpu::Nvidia {
            return Err(coded(
                Code::InvalidInput,
                "the desktop cannot draw on the NVIDIA GPU and leave it alone: choose intel or auto",
            ));
        }
        if input.dry_run {
            let plan = Plan::load(None)?;
            return Ok(dry_run(&gpu::set_desktop(
                &plan.machine(),
                choice.into(),
                input.release_nvidia,
            )?));
        }
        let release = input.release_nvidia;
        Ok(Changes {
            lines: self.system(async move |p| {
                p.set_primary_gpu(gpu::DesktopGpu::from(choice).as_str(), release)
                    .await
            })?,
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
        Ok(agents::config_files(&home(), input.project.get()?).into_wire())
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
        Ok(agents::set(
            &path,
            &input.key,
            agents::value_of(&input.value),
            input.dry_run,
        )?
        .into_wire())
    }

    fn runnables_check(&self, _: &Ctx, input: RunnablesCheckIn) -> Result<RunnablesCheck> {
        let sources = Sources::resolve(&Dirs::from_env(), Some(input.project.get()?))?;
        let team = match &input.team {
            Some(name) => Some((
                name.as_str(),
                sources.team_path(name).with_context(|| {
                    format!(
                        "no team {name} in .legionctl/teams, the user's teams or the plugin's teams"
                    )
                })?,
            )),
            None => None,
        };
        Ok(sources
            .check(
                team.as_ref().map(|(n, p)| (*n, p.as_path())),
                input.harness.into(),
                false,
            )?
            .into_wire())
    }

    fn runnables_show(&self, _: &Ctx, input: RunnablesShowIn) -> Result<RunnableGrant> {
        Ok(dossiers()?.grant_of(&input.target)?.into_wire())
    }

    fn agents_usage(&self, _: &Ctx, input: AgentUsageIn) -> Result<UsageReport> {
        Ok(agents::usage(&home().join(".claude"), &home().join(".codex"), input.days).into_wire())
    }

    fn agents_plugins_list(&self, _: &Ctx, _: Empty) -> Result<Vec<Plugin>> {
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

    fn agents_plugins_install(&self, _: &Ctx, input: PluginInstallIn) -> Result<Lines> {
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

    fn agents_accounts_list(&self, _: &Ctx, _: Empty) -> Result<Vec<Account>> {
        let mut out: Vec<Account> = Vec::new();
        for held in legionctl_core::accounts::logins()? {
            let i = out
                .iter()
                .position(|a| a.name == held.name && a.source == held.source)
                .unwrap_or_else(|| {
                    out.push(Account {
                        name: held.name.clone(),
                        source: held.source.into(),
                        active: held.active,
                        claude: false,
                        codex: false,
                    });
                    out.len() - 1
                });
            let a = &mut out[i];
            a.active |= held.active;
            if held.agent == "claude" {
                a.claude = true;
            } else {
                a.codex = true;
            }
        }
        Ok(out)
    }

    fn agents_accounts_capture(&self, _: &Ctx, input: AccountIn) -> Result<Lines> {
        crate::accounts::capture(&input.name)
    }

    fn agents_accounts_usage(
        &self,
        _: &Ctx,
        input: legionctl_api::AccountUsageIn,
    ) -> Result<Vec<legionctl_api::AccountUsage>> {
        Ok(crate::accounts::usage(&self.runtime, &input)?.into_wire())
    }

    fn agents_accounts_login(&self, _: &Ctx, input: AccountLoginIn) -> Result<Lines> {
        crate::accounts::login(&input)
    }

    fn agents_accounts_switch(&self, _: &Ctx, input: AccountIn) -> Result<Lines> {
        crate::accounts::switch(&input.name)
    }

    fn workspace_open(&self, _: &Ctx, input: WorkspaceOpenIn) -> Result<Workspace> {
        let root = config(None)?.config.workspace.root;
        Ok(dossiers()?
            .open_workspace(
                dossier_of(&input.session)?,
                input.repo.get()?,
                input.base.as_deref(),
                input.topic.as_deref(),
                &root,
            )?
            .into_wire())
    }

    fn workspace_status(&self, _: &Ctx, input: WorkspaceIn) -> Result<WorkspaceStatus> {
        let store = dossiers()?;
        let ws = store.workspace_of(
            &dossier_of(&input.session)?,
            input.repo.get()?,
            input.topic.as_deref(),
        )?;
        Ok(store.workspace_status(&ws)?.into_wire())
    }

    fn workspace_list(&self, _: &Ctx, _: Empty) -> Result<Workspaces> {
        Ok(dossiers()?.workspaces(None)?.into_wire())
    }

    fn workspace_checkpoint(&self, ctx: &Ctx, input: CheckpointIn) -> Result<Lines> {
        let store = dossiers()?;
        let session = session(&input.session)?;
        let ws = store.workspace_of(
            &dossier_of(&input.session)?,
            input.repo.get()?,
            input.topic.as_deref(),
        )?;
        let accepted = store.accept_tests(&ws.path, &input.accept_test, &input.reason, session)?;
        for (test, (accepted, reason)) in input.accept_test.iter().zip(accepted) {
            Db::open_default()?.record(
                &ctx.actor,
                "code.tests.accept",
                &serde_json::json!({"test": test, "accepted": accepted, "reason": reason, "workspace": ws.path, "session": session}),
            )?;
        }
        if let Some(held) = store.checkpoint_gate(session)? {
            anyhow::bail!(held);
        }
        let jail = jail(ctx, &store, Some(&input.repo))?;
        let lines = store.checkpoint(&ws, &input.message, (session, jail.as_ref()))?;
        // the commit's patch id links it to this checkpoint, also after `workspace merge` rebased it; a folder
        // workspace has no commits
        let patch = if ws.is_folder() {
            None
        } else {
            legionctl_core::codegraph::patch_ids(&ws.path, "HEAD^!")?.pop()
        };
        if let Some(patch) = &patch {
            Db::open_default()?.record(
                &ctx.actor,
                legionctl_core::codegraph::COMMIT_EVENT,
                &serde_json::json!({"patch_id": patch.0, "commit": patch.1, "session": session, "workspace": ws.path}),
            )?;
        }
        crate::code::trigger(&ws.path);
        store.thread_checkpoint(
            session,
            patch.as_ref().map(|p| p.1.as_str()),
            &input.message,
        )?;
        crate::thread::wake_due();
        Ok(Lines { lines })
    }

    fn code_tests_run(&self, ctx: &Ctx, input: TestsRunIn) -> Result<Lines> {
        let repo = input.repo.get()?;
        let store = dossiers()?;
        let jail = jail(ctx, &store, Some(&input.repo))?;
        let db = Db::open_default()?;
        let job = db.job_begin(
            "code.tests.run",
            &serde_json::json!({"repo": repo, "filter": input.filter}),
        )?;
        let out = store.run_tests(
            repo,
            (input.filter.as_deref(), jail.as_ref()),
            job,
            |counts| {
                let _ = db.job_step(
                    job,
                    &serde_json::json!({"repo": repo, "filter": input.filter, "results": counts}),
                );
            },
        );
        let out = out.map(|lines| Lines { lines });
        end_job(&db, job, &out)?;
        out
    }

    fn code_checks_detect(&self, _: &Ctx, input: RepoIn) -> Result<Lines> {
        Ok(Lines {
            lines: legionctl_core::checks::detect(input.repo.get()?)?,
        })
    }

    fn code_checks_list(&self, _: &Ctx, input: RepoIn) -> Result<Lines> {
        Ok(Lines {
            lines: dossiers()?.checks_list(input.repo.get()?)?,
        })
    }

    fn code_checks_run(&self, ctx: &Ctx, input: ChecksRunIn) -> Result<Lines> {
        let session = Some(input.session.0.as_str()).filter(|s| !s.is_empty());
        let store = dossiers()?;
        let jail = jail(ctx, &store, Some(&input.repo))?;
        let runs = store.run_checks(input.repo.get()?, &input.checks, (session, jail.as_ref()))?;
        Ok(Lines {
            lines: runs
                .iter()
                .map(|r| {
                    format!(
                        "{}: exit {} in {} ms (run {}; log {})",
                        r.check,
                        r.exit,
                        r.profile.wall_ms,
                        r.id,
                        r.log().display()
                    )
                })
                .collect(),
        })
    }

    fn code_checks_show(&self, _: &Ctx, input: CheckShowIn) -> Result<Lines> {
        let (run, tail) = dossiers()?.check_run(input.repo.get()?, &input.run)?;
        let p = run.profile;
        Ok(Lines {
            lines: [
                format!(
                    "run {}: check {} ({}), exit {}, tree {}",
                    run.id,
                    run.check,
                    run.kind.name(),
                    run.exit,
                    run.tree
                ),
                format!("command: {}", run.command),
                format!(
                    "profile: wall {} ms, cpu {} ms, max rss {} KiB, read {} B, written {} B",
                    p.wall_ms, p.cpu_ms, p.max_rss_kib, p.read_bytes, p.write_bytes
                ),
                format!("folder: {}", run.folder.display()),
            ]
            .into_iter()
            .chain(tail.lines().map(String::from))
            .collect(),
        })
    }

    fn code_tests_list(&self, _: &Ctx, input: RepoIn) -> Result<Vec<TestRow>> {
        Ok(dossiers()?.tests_list(input.repo.get()?)?.into_wire())
    }

    fn code_tests_show(&self, _: &Ctx, input: TestShowIn) -> Result<TestDetail> {
        Ok(dossiers()?
            .test_show(input.repo.get()?, &input.name)?
            .into_wire())
    }

    fn code_tests_review(&self, _: &Ctx, input: TestReviewIn) -> Result<TestReview> {
        let side = if input.staged {
            Side::Index(input.index.as_ref().map(ClientPath::get).transpose()?)
        } else {
            Side::WorkTree
        };
        Ok(dossiers()?
            .review_tests(input.repo.get()?, side)?
            .into_wire())
    }

    fn code_tests_mutant_record(&self, _: &Ctx, input: MutantRecordIn) -> Result<Lines> {
        Ok(Lines {
            lines: vec![dossiers()?.record_mutant(
                input.repo.get()?,
                &input.file,
                input.line,
                &input.description,
                &input.killed_by,
            )?],
        })
    }

    fn code_tests_mutant_run(&self, _: &Ctx, input: WorkspaceIn) -> Result<Lines> {
        let store = dossiers()?;
        let ws = store.workspace_of(
            &dossier_of(&input.session)?,
            input.repo.get()?,
            input.topic.as_deref(),
        )?;
        let db = Db::open_default()?;
        let job_input = serde_json::json!({"workspace": ws.path, "base": ws.base});
        let job = db.job_begin("code.tests.mutant.run", &job_input)?;
        let out = store
            .run_mutants(&ws.path, &ws.base, |done, total| {
                let mut step = job_input.clone();
                step["progress"] = serde_json::json!({"done": done, "total": total});
                let _ = db.job_step(job, &step);
            })
            .map(|lines| Lines { lines });
        end_job(&db, job, &out)?;
        out
    }

    fn workspace_merge(&self, ctx: &Ctx, input: MergeIn) -> Result<Lines> {
        let store = dossiers()?;
        let (dir, topic) = (input.repo.get()?, input.topic.as_deref());
        // no agent session: the user, whose merge of a branch that sets its own gates the desktop dialog confirms
        let (ws, session) = if input.session.0.is_empty() {
            let ws = store.workspace_for_user(input.dossier.as_deref(), dir, topic)?;
            (ws, None)
        } else {
            anyhow::ensure!(
                input.dossier.is_none(),
                "--dossier is for the user's merge; an agent session merges the workspace of its own dossier"
            );
            (
                store.workspace_of(&dossier_of(&input.session)?, dir, topic)?,
                Some(input.session.0.as_str()),
            )
        };
        let jail = jail(ctx, &store, Some(&input.repo))?;
        let lines = store.merge(&ws, (session, jail.as_ref()), |what| {
            self.runtime
                .block_on(crate::desktop::ask("legionctl: confirm a merge", what))
                .context("the merge changes .legionctl/ and needs the user's confirmation in a desktop dialog")
        })?;
        crate::code::trigger(&ws.repo);
        Ok(Lines { lines })
    }

    fn workspace_close(&self, _: &Ctx, input: CloseIn) -> Result<Lines> {
        let store = dossiers()?;
        let ws = store.workspace_of(
            &dossier_of(&input.session)?,
            input.repo.get()?,
            input.topic.as_deref(),
        )?;
        Ok(Lines {
            lines: vec![store.close(&ws, input.force, input.dry_run)?],
        })
    }

    fn setup_check(&self, _: &Ctx, _: Empty) -> Result<SetupReport> {
        Ok(setup::check(
            &Sources::resolve(&Dirs::from_env(), None)?,
            &config(None)?.config.session,
        )?
        .into_wire())
    }

    fn setup_apply(&self, _: &Ctx, input: SetupApplyIn) -> Result<Changes> {
        Ok(Changes {
            lines: setup::apply(
                input.install_ctx,
                &Sources::resolve(&Dirs::from_env(), None)?,
                &config(None)?.config.session,
            )?,
            dry_run: false,
        })
    }

    fn setup_turn_on(&self, _: &Ctx, input: ServiceIn) -> Result<Changes> {
        Ok(Changes {
            lines: setup::turn_on(&input.service)?,
            dry_run: false,
        })
    }

    fn reminders_list(&self, _: &Ctx, input: ProjectIn) -> Result<Reminders> {
        Ok(reminders::list(&config(Some(&input.project))?).into_wire())
    }

    fn reminders_on(&self, _: &Ctx, input: IdIn) -> Result<Reminders> {
        reminders::set_enabled(&config(Some(&input.project))?, &input.id, true)?;
        Ok(reminders::list(&config(Some(&input.project))?).into_wire())
    }

    fn reminders_off(&self, _: &Ctx, input: IdIn) -> Result<Reminders> {
        reminders::set_enabled(&config(Some(&input.project))?, &input.id, false)?;
        Ok(reminders::list(&config(Some(&input.project))?).into_wire())
    }

    fn reminders_add(&self, _: &Ctx, input: ReminderAddIn) -> Result<Reminders> {
        reminders::add(
            &config(Some(&input.project))?,
            &input.id,
            &input.text,
            input.event.into(),
        )?;
        Ok(reminders::list(&config(Some(&input.project))?).into_wire())
    }

    fn reminders_rm(&self, _: &Ctx, input: IdIn) -> Result<Reminders> {
        reminders::remove(&config(Some(&input.project))?, &input.id)?;
        Ok(reminders::list(&config(Some(&input.project))?).into_wire())
    }

    fn hooks_list(&self, _: &Ctx, input: ProjectIn) -> Result<HookTable> {
        Ok(Self::hook_table(&config(Some(&input.project))?))
    }

    fn hooks_bind(&self, _: &Ctx, input: HookBindIn) -> Result<HookTable> {
        hooks::bind(
            &config(Some(&input.project))?,
            input.event.into(),
            &input.action,
        )?;
        Ok(Self::hook_table(&config(Some(&input.project))?))
    }

    fn hooks_unbind(&self, _: &Ctx, input: HookBindIn) -> Result<HookTable> {
        hooks::unbind(
            &config(Some(&input.project))?,
            input.event.into(),
            &input.action,
        )?;
        Ok(Self::hook_table(&config(Some(&input.project))?))
    }

    fn hooks_add(&self, _: &Ctx, input: HookAddIn) -> Result<HookTable> {
        hooks::add_command(
            &config(Some(&input.project))?,
            &input.id,
            &input.command,
            input.timeout_s,
        )?;
        Ok(Self::hook_table(&config(Some(&input.project))?))
    }

    fn hooks_rm(&self, _: &Ctx, input: IdIn) -> Result<HookTable> {
        hooks::remove_action(&config(Some(&input.project))?, &input.id)?;
        Ok(Self::hook_table(&config(Some(&input.project))?))
    }

    fn hooks_run(&self, ctx: &Ctx, input: HookRunIn) -> Result<HookReply> {
        if let Some(reply) = unmigrated(input.event.into()) {
            return Ok(reply);
        }
        let resolved = config(Some(&input.project))?;
        let store = Dossiers::open(&resolved.config.dossier)?;
        let session = input.session.as_ref().filter(|s| !s.0.is_empty());
        if input.again {
            // the reply goes on because a stop hook stopped it: only a thread hard-block stops it again
            return Ok(HookReply {
                text: String::new(),
                block: match session {
                    Some(s) => store.thread_stop(&s.0, true)?,
                    None => None,
                },
            });
        }
        let mut active = match session {
            Some(s) => store.active(&s.0)?,
            None => None,
        };
        if let (Some(s), Some(_)) = (session, &active) {
            // each hook call shows the session is live, so its dossier is not taken over
            store.touch(&s.0)?;
        }
        let (linked, rebound) = bind_linked(&store, &resolved, &input, &mut active)?;
        let place = match active.as_deref().zip(input.project.get().ok()) {
            Some((d, dir)) => store.place(d, dir)?,
            None => None,
        };
        let failed = match &rebound {
            Some(Err(why)) => Some(why.as_str()),
            _ => None,
        };
        if let Some(block) = blocking_reason(
            &resolved,
            &input,
            (linked.as_ref(), failed),
            active.as_deref(),
            place.as_ref(),
        )? {
            return Ok(HookReply {
                text: String::new(),
                block: Some(block),
            });
        }
        let mut lines = Vec::new();
        if let Some(s) = session {
            let (block, said) = thread_hooks(&store, &s.0, &input)?;
            if block.is_some() {
                return Ok(HookReply {
                    text: String::new(),
                    block,
                });
            }
            lines.extend(said);
        }
        if let (Some(s), Some(d), Some(p)) = (session, active.as_deref(), place.as_ref())
            && let Some(told) =
                dossier_nudge(ctx, &resolved, &store, (&s.0, d, p), input.event.into())?
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
        if Event::from(input.event) == Event::SessionStart
            && let Ok(dir) = input.project.get()
        {
            crate::code::trigger(dir);
        }
        if Event::from(input.event) == Event::SessionStart
            && let Some(s) = session
        {
            lines.extend(start_lines(
                &resolved,
                &store,
                (&s.0, input.source.as_deref() == Some("compact")),
                active.as_deref(),
                (linked.is_some(), rebound),
                place.as_ref(),
            ));
        } else if let Some(Ok(line)) = rebound {
            lines.push(line);
        }
        let make = || jail(ctx, &store, Some(&input.project));
        let mut jail = hooks::Jail::new(&make);
        lines.push(hooks::run(
            &resolved,
            input.event.into(),
            active.is_some(),
            &mut jail,
        ));
        lines.retain(|l| !l.is_empty());
        Ok(HookReply {
            text: lines.join("\n"),
            block: None,
        })
    }

    fn hooks_delegate(&self, _: &Ctx, input: SessionIn) -> Result<HookReply> {
        let active = match input.session.0.as_str() {
            "" => None,
            s => dossiers()?.active(s)?,
        };
        Ok(HookReply {
            text: active.map(|d| delegation_rule(&d)).unwrap_or_default(),
            block: None,
        })
    }

    fn dossier_create(&self, ctx: &Ctx, input: legionctl_api::DossierCreateIn) -> Result<Lines> {
        let folder = path(input.folder.as_ref())?;
        let lines = dossiers()?.create_new(&input.name, folder)?;
        Db::open_default()?.record(
            &ctx.actor,
            "dossier.create",
            &serde_json::json!({"name": input.name, "folder": folder}),
        )?;
        Ok(Lines { lines })
    }

    fn dossier_use(&self, ctx: &Ctx, input: DossierUseIn) -> Result<Lines> {
        if input.session.0.is_empty() {
            dossiers()?.known(&input.name)?;
            user_dossier_set(ctx, &input.repo, Some(&input.name))?;
            return Ok(Lines {
                lines: vec![format!(
                    "the user's dossier: {} (the app's new chats and agent runs work in it)",
                    input.name
                )],
            });
        }
        let lines = dossiers()?.bind(
            session(&input.session)?,
            &ctx.actor,
            &input.name,
            input.steal,
            Some(&place_of(input.repo.get()?)),
        )?;
        Ok(Lines { lines })
    }

    fn dossier_active(&self, _: &Ctx, input: SessionIn) -> Result<Lines> {
        if input.session.0.is_empty() {
            return Ok(Lines {
                lines: vec![match user_dossier()? {
                    Some(d) => format!("the user's dossier: {d}"),
                    None => "the user has no dossier: choose one with dossier use <name> (dossier list), or make one \
                             with dossier create <name>"
                        .into(),
                }],
            });
        }
        let id = session(&input.session)?;
        let active = dossiers()?.active(id)?;
        Ok(Lines {
            lines: vec![format!(
                "session {id}: active dossier {}",
                active.as_deref().unwrap_or("(none)")
            )],
        })
    }

    fn dossier_release(&self, ctx: &Ctx, input: DossierReleaseIn) -> Result<Lines> {
        if input.session.0.is_empty() {
            let had = user_dossier()?;
            user_dossier_set(ctx, &ClientPath(PathBuf::from("/")), None)?;
            return Ok(Lines {
                lines: vec![had.map_or_else(
                    || "the user had no dossier".into(),
                    |d| format!("the user's dossier {d} is no longer chosen"),
                )],
            });
        }
        let store = dossiers()?;
        // the session-end hook: a pool whose main agent ended stops (flow.stop `main0 ended`); /clear and /resume
        // end only this session of the agent
        if !matches!(input.reason.as_deref(), Some("clear" | "resume")) {
            store.main_ended(session(&input.session)?)?;
        }
        let released = store.release(session(&input.session)?)?;
        Ok(Lines {
            lines: vec![
                released.map_or_else(|| "no active dossier".into(), |d| format!("released {d}")),
            ],
        })
    }

    fn dossier_locks(&self, _: &Ctx, _: Empty) -> Result<Locks> {
        Ok(dossiers()?.locks()?.into_wire())
    }

    fn dossier_list(&self, _: &Ctx, _: Empty) -> Result<DossierList> {
        let chosen = user_dossier()?;
        let mut list = dossiers()?.list()?;
        for d in &mut list {
            d.chosen = chosen.as_ref() == Some(&d.name);
        }
        Ok(list.into_wire())
    }

    fn dossier_migrate(&self, _: &Ctx, input: DossierMigrateIn) -> Result<Lines> {
        use legionctl_core::migrate;
        let config = config(None)?.config.dossier;
        if input.dry_run || !migrate::pending() {
            return Ok(Lines {
                lines: migrate::plan(&config)?,
            });
        }
        let _running = match MIGRATING.try_lock() {
            Ok(held) => held,
            Err(std::sync::TryLockError::Poisoned(held)) => held.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                return Err(coded(
                    Code::Conflict,
                    "a dossier migration runs already; see legionctl jobs list",
                ));
            }
        };
        let db = Db::open_default()?;
        let job = db.job_begin(migrate::JOB, &serde_json::json!({}))?;
        let out = migrate::run(&config, &db);
        end_job(&db, job, &out)?;
        Ok(Lines {
            lines: migrate::summary(&out?),
        })
    }

    fn dossier_rename(&self, ctx: &Ctx, input: DossierRenameIn) -> Result<Lines> {
        let lines = dossiers()?.rename(&input.name, &input.to)?;
        Db::open_default()?.record(
            &ctx.actor,
            "dossier.rename",
            &serde_json::json!({"from": input.name, "to": input.to}),
        )?;
        Ok(Lines { lines })
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
        )?;
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
        let store = dossiers()?;
        let me = session(&input.session)?;
        if let Some(member) = &input.member {
            return Ok(Lines {
                lines: store.backfill(
                    &Db::open_default()?,
                    me,
                    member,
                    &input.path,
                    &input.content.0,
                )?,
            });
        }
        let path = store.append(me, &input.path, &input.content.0)?;
        Ok(Lines {
            lines: vec![path.display().to_string()],
        })
    }

    fn dossier_edit(&self, _: &Ctx, input: DossierEditIn) -> Result<Lines> {
        let lines = dossiers()?.edit(
            session(&input.session)?,
            &input.path,
            input.mode.into(),
            &input.start,
            input.end.as_deref(),
            &input.content.0,
        )?;
        Ok(Lines { lines })
    }

    fn about(&self, _: &Ctx, _: Empty) -> Result<Vec<legionctl_api::AboutItem>> {
        let item = |part: &str, value: String, ok: bool| legionctl_api::AboutItem {
            part: part.into(),
            value,
            state: if ok { "ok" } else { "warn" }.into(),
        };
        let mut items = vec![item(
            "operator",
            format!(
                "legionctl {} (API build {})",
                env!("CARGO_PKG_VERSION"),
                legionctl_api::api_fingerprint()
            ),
            true,
        )];
        let ours = legionctl_machine::interface_version();
        items.push(match self.system(async |p| p.version().await) {
            Ok(v) if v == ours => item("daemon", format!("legionctl-system, interface {v}: the same build"), true),
            Ok(v) => item(
                "daemon",
                format!(
                    "legionctl-system, interface {v}; this operator has {ours}: install the matching package and \
                     restart both"
                ),
                false,
            ),
            Err(e) => item("daemon", format!("{e:#}"), false),
        });
        items.push(match Plan::load(None) {
            Ok(plan) => item("machine", format!("profile {}", plan.profile.id), true),
            Err(e) => item("machine", format!("no machine profile: {e:#}"), false),
        });
        items.push(item(
            "licenses",
            "MIT; the notices of the parts from other projects: /usr/share/licenses/legionctl"
                .into(),
            true,
        ));
        items.push(item(
            "source",
            "https://github.com/ksquarekumar/lenovolegionlinux".into(),
            true,
        ));
        Ok(items)
    }

    fn approval_answer(&self, ctx: &Ctx, input: legionctl_api::ApprovalAnswerIn) -> Result<Lines> {
        // the question is the user's: an agent never answers one, also not after the user allowed the call
        anyhow::ensure!(
            ctx.principal == legionctl_api::Principal::User,
            "only the user answers a question"
        );
        let allow = input.answer == legionctl_api::Answer::Allow;
        crate::desktop::answer(input.ask, allow)?;
        Ok(Lines {
            lines: vec![format!(
                "question {}: {}",
                input.ask,
                if allow { "allowed" } else { "denied" }
            )],
        })
    }

    fn feedback_new(&self, _: &Ctx, input: legionctl_api::FeedbackIn) -> Result<Lines> {
        let store = dossiers()?;
        let now = legionctl_core::facts::now_ms() / 1000;
        let daemon = match self.system(async |p| p.version().await) {
            Ok(v) if v == legionctl_machine::interface_version() => format!("{v} (matches)"),
            Ok(v) => format!(
                "{v} (this operator: {}; restart both)",
                legionctl_machine::interface_version()
            ),
            Err(e) => format!("does not answer: {e:#}"),
        };
        let screenshot = if input.screenshot {
            Some(self.runtime.block_on(crate::desktop::screenshot())?)
        } else {
            None
        };
        let mut head = vec![
            "class: feedbacks".to_string(),
            "immutable: true".to_string(),
            format!("time: {}", legionctl_core::views::utc(now)),
            format!(
                "operator: {} api {}",
                env!("CARGO_PKG_VERSION"),
                legionctl_api::api_fingerprint()
            ),
            format!("daemon: {daemon}"),
        ];
        head.extend(
            input
                .about
                .iter()
                .map(|(k, v)| format!("{k}: {}", v.replace('\n', " "))),
        );
        // the app's state, and the operator's last events
        let mut state: serde_json::Value = match &input.state {
            Some(text) => serde_json::from_str(text).context("the state must be JSON")?,
            None => serde_json::json!({}),
        };
        let db = Db::open_default()?;
        let after = db
            .last_event_id()?
            .saturating_sub(i64::from(FEEDBACK_EVENTS));
        let (events, _) = db.events_page(&EventFilter::default(), after, FEEDBACK_EVENTS)?;
        state["events"] = serde_json::to_value(events.into_wire())?;
        let state = serde_json::to_string_pretty(&state)?;
        let mut files: Vec<(&str, &[u8])> = vec![("state.json", state.as_bytes())];
        if let Some(png) = &screenshot {
            files.push(("screenshot.png", png));
        }
        let links: Vec<&str> = files.iter().map(|(name, _)| *name).collect();
        head.push(format!("links: [{}]", links.join(", ")));
        let note = format!(
            "---\n{}\n---\n# Feedback\n\n{}\n",
            head.join("\n"),
            input.note.trim()
        );
        files.insert(0, ("note.md", note.as_bytes()));
        let slug: String = input
            .note
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|w| !w.is_empty())
            .take(5)
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>()
            .join("-");
        let slug = if slug.is_empty() { "note".into() } else { slug };
        let rel = store.feedback(&input.dossier, &slug, &files)?;
        Ok(Lines {
            lines: vec![format!("feedback kept in {rel}")],
        })
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

    fn dossier_nudges(&self, _: &Ctx, input: DossierNudgesIn) -> Result<Page<EventRow>> {
        let (after, limit) = page_of(input.cursor.as_deref(), input.limit)?;
        let filter = EventFilter {
            kinds: &["dossier.nudge", "dossier.ack"],
            field: input.of.as_deref().map(|s| ("$.session", s)),
            naming: None,
        };
        Ok(page(
            Db::open_default()?.events_page(&filter, after, limit)?,
        ))
    }

    fn dossier_graph(&self, _: &Ctx, input: DossierGraphIn) -> Result<Graph> {
        let ws = dossiers()?;
        let dossier = match input.dossier {
            Some(d) => d,
            None => dossier_of(&input.session)?,
        };
        Ok(ws.graph(&dossier, input.node.as_deref())?.into_wire())
    }

    fn dossier_check(&self, _: &Ctx, input: DossierCheckIn) -> Result<DossierFindings> {
        let ws = dossiers()?;
        let dossier = match input.dossier {
            Some(d) => d,
            None => dossier_of(&input.session)?,
        };
        Ok(DossierFindings {
            problems: ws.check(&dossier),
            dossier,
        })
    }

    fn dossier_health(&self, _: &Ctx, input: legionctl_api::DossierHealthIn) -> Result<Lines> {
        let store = dossiers()?;
        let dossier = match input.dossier {
            Some(d) => d,
            None => dossier_of(&input.session)?,
        };
        Ok(Lines {
            lines: store.health(&dossier, usize::try_from(input.limit)?),
        })
    }

    fn hooks_compact(&self, _: &Ctx, input: legionctl_api::CompactIn) -> Result<HookReply> {
        let store = dossiers()?;
        let session = session(&input.session)?;
        let Some(dossier) = store.active(session)? else {
            return Ok(HookReply::default());
        };
        let trigger = input.trigger.as_deref().unwrap_or("auto");
        let health = store.add(
            session,
            "HEALTH",
            "before-compaction",
            &format!(
                "---\nclass: HEALTH\nstatus: recorded\n---\n# Health before a compaction ({trigger})\n\n{}\n",
                store.health(&dossier, 40).join("\n")
            ),
        )?;
        let journal = store.add(
            session,
            "JOURNAL",
            "compaction",
            &compaction_journal(&store, &dossier, session, trigger, &health)?,
        )?;
        Ok(HookReply {
            text: format!("legionctl: wrote {journal} and {health}"),
            block: None,
        })
    }

    fn dossier_guard(&self, _: &Ctx, input: GuardIn) -> Result<Guard> {
        use legionctl_core::migrate;
        // the old root takes writes only before the migration: not while it moves the dossiers, nor after it
        let migrating = matches!(
            MIGRATING.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        );
        if migrate::in_legacy(Path::new(&input.path)) && (migrating || !migrate::pending()) {
            return Ok(Guard {
                refusal: Some(format!(
                    "{} is in the old dossier root, which the dossier migration {}: write the dossier in its home \
                     (legionctl dossier active)",
                    input.path,
                    if migrating { "moves now" } else { "moved away" }
                )),
            });
        }
        Ok(Guard {
            refusal: dossiers()?.write_refusal(session(&input.session)?, Path::new(&input.path))?,
        })
    }

    fn skills(&self, _: &Ctx, input: ProjectIn) -> Result<SkillList> {
        Ok(skills::inventory(&home(), &config(Some(&input.project))?).into_wire())
    }

    fn monitor(&self, _: &Ctx, input: MonitorIn) -> Result<Samples> {
        anyhow::ensure!((1..=3600).contains(&input.count), "count must be 1-3600");
        anyhow::ensure!(
            (100..=60_000).contains(&input.interval_ms),
            "--interval-ms must be 100-60000"
        );
        let window = std::time::Duration::from_millis(u64::from(input.interval_ms));
        (0..input.count)
            .map(|_| Ok(self.sample(window)?.into_wire()))
            .collect()
    }

    fn sensors(&self, _: &Ctx, _: SensorsIn) -> Result<Readings> {
        Ok(self.readings()?.into_wire())
    }

    fn events(&self, _: &Ctx, input: EventsIn) -> Result<Page<EventRow>> {
        let (after, limit) = page_of(input.cursor.as_deref(), input.limit)?;
        Ok(page(Db::open_default()?.events_page(
            &EventFilter::default(),
            after,
            limit,
        )?))
    }

    fn packages_list(&self, _: &Ctx, _: Empty) -> Result<Packages> {
        Ok(Store::new(Store::default_root())
            .list(&config(None)?.config.packages)?
            .into_wire())
    }

    fn packages_fetch(&self, _: &Ctx, input: PackageFetchIn) -> Result<Manifest> {
        let resolved = config(None)?;
        let package = resolved
            .config
            .packages
            .get(&input.name)
            .with_context(|| format!("no package {} in the config (packages)", input.name))?;
        Ok(Store::new(Store::default_root())
            .fetch(
                &input.name,
                package,
                input.force,
                &resolved.config.session.hidden,
            )?
            .into_wire())
    }

    fn gateway_usage(&self, _: &Ctx, input: GatewayUsageIn) -> Result<Page<GatewayUsage>> {
        let (after, limit) = page_of(input.cursor.as_deref(), input.limit)?;
        Ok(page(Db::open_default()?.gateway_usage_page(after, limit)?))
    }

    fn cache_get(&self, ctx: &Ctx, input: CacheGetIn) -> Result<CacheValue> {
        let key = tool_key(ctx, &input.call)?;
        // a cache that cannot be read is a reported miss (user decision Q2)
        let value = match Cache::open().and_then(|c| c.get("tools", &key)) {
            Ok(entry) => {
                crate::faults::works("cache-tools");
                entry.map(|e| String::from_utf8_lossy(&e.value).into_owned())
            }
            Err(e) => {
                crate::faults::failed(
                    "cache-tools",
                    "the tool cache fails, so its calls go upstream",
                    &e,
                );
                None
            }
        };
        Ok(CacheValue { value })
    }

    fn cache_put(&self, ctx: &Ctx, input: CachePutIn) -> Result<Empty> {
        let key = tool_key(ctx, &input.call)?;
        let entry = legionctl_core::cache::Entry {
            value: input.value.into_bytes(),
            meta: serde_json::Value::Null,
        };
        Cache::open()?.put(("tools", &key), &entry, input.ttl_s, cache_limit("tools")?)?;
        Ok(Empty {})
    }

    fn cache_stats(&self, _: &Ctx, _: Empty) -> Result<Vec<CacheStat>> {
        let limits = config(None)?.config.cache;
        let usage = Cache::open()?.usage()?;
        Ok(limits
            .iter()
            .map(|(ns, limit)| {
                let used = usage.iter().find(|u| &u.ns == ns);
                CacheStat {
                    ns: ns.clone(),
                    entries: used.map_or(0, |u| u.entries),
                    bytes: used.map_or(0, |u| u.bytes),
                    max_bytes: limit.max_bytes,
                }
            })
            .collect())
    }

    fn cache_purge(&self, _: &Ctx, input: CachePurgeIn) -> Result<Lines> {
        let deleted = Cache::open()?.purge(input.ns.as_deref())?;
        Ok(Lines {
            lines: vec![format!(
                "{deleted} entries removed from {}",
                input.ns.as_deref().unwrap_or("every namespace")
            )],
        })
    }

    // the operator's audit row of this call is the record
    fn tools_record(&self, _: &Ctx, _: ToolCall) -> Result<Empty> {
        Ok(Empty {})
    }

    fn tools_calls(&self, _: &Ctx, input: ToolCallsIn) -> Result<Page<ToolCallRow>> {
        let (after, limit) = page_of(input.cursor.as_deref(), input.limit)?;
        let filter = EventFilter {
            kinds: &["internal.tools.record"],
            field: input.session.as_deref().map(|s| ("$.input.session", s)),
            naming: None,
        };
        let (rows, next) = Db::open_default()?.events_page(&filter, after, limit)?;
        Ok(Page {
            items: rows
                .into_iter()
                .map(|e| {
                    Ok(ToolCallRow {
                        id: e.id,
                        at: e.ts,
                        call: serde_json::from_value(e.payload["input"].clone())?,
                    })
                })
                .collect::<Result<_>>()?,
            next: next.map(|n| n.to_string()),
        })
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
        Ok(scan.into_wire())
    }

    fn images_build(&self, _: &Ctx, input: RepoIn) -> Result<Lines> {
        let repo = input.repo.get()?;
        let db = Db::open_default()?;
        let job = db.job_begin("images.build", &serde_json::json!({"repo": repo}))?;
        let out = build_images(repo);
        end_job(&db, job, &out)?;
        out
    }

    fn workspace_pack(&self, _: &Ctx, input: PackIn) -> Result<Lines> {
        let repo = input.repo.get()?;
        let session = config(Some(&input.repo))?.config.session;
        let db = Db::open_default()?;
        let job_input = serde_json::json!({"repo": repo, "service": input.service, "as": input.kind, "gpu": input.gpu});
        let job = db.job_begin("workspace.pack", &job_input)?;
        let out = pack_job(&job_input, &session);
        end_job(&db, job, &out)?;
        out
    }

    fn jobs_list(&self, _: &Ctx, input: PageIn) -> Result<Page<legionctl_api::Job>> {
        let (after, limit) = page_of(input.cursor.as_deref(), input.limit)?;
        Ok(page(Db::open_default()?.jobs_page(after, limit)?))
    }

    fn jobs_show(&self, _: &Ctx, input: JobIdIn) -> Result<legionctl_api::Job> {
        Ok(Db::open_default()?
            .job(input.id)?
            .ok_or_else(|| {
                coded(
                    Code::NotFound,
                    format!("no job {}; see legionctl jobs list", input.id),
                )
            })?
            .into_wire())
    }

    fn images_run(&self, _: &Ctx, input: RepoIn) -> Result<Lines> {
        let repo = input.repo.get()?;
        let (project, tag) = (
            legionctl_core::images::project_name(repo),
            legionctl_core::images::tag(repo)?,
        );
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
        let project = legionctl_core::images::project_name(input.repo.get()?);
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
        self.runtime
            .block_on(crate::virt::status(&SysRoot::from_env(), low))
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
                "created; start it with legionctl virt act start <name>"
            }
            .into(),
        );
        Ok(Lines { lines })
    }

    fn battery_status(&self, _: &Ctx, _: Empty) -> Result<legionctl_api::BatteryState> {
        let battery = legionctl_machine::battery::state(&SysRoot::from_env())?;
        let filter = legionctl_core::db::EventFilter {
            kinds: &["battery.sample"],
            ..Default::default()
        };
        let (rows, _) = Db::open_default()?.events_page(&filter, 0, 1000)?;
        let health_trend = rows
            .iter()
            .filter_map(|e| Some((e.ts, e.payload["health_pct"].as_f64()?)))
            .collect();
        Ok(legionctl_api::BatteryState {
            battery: battery.into_wire(),
            health_trend,
        })
    }

    fn battery_mode(&self, _: &Ctx, input: legionctl_api::BatteryModeIn) -> Result<Changes> {
        let mode = match input.mode {
            legionctl_api::BatteryMode::Standard => "standard",
            legionctl_api::BatteryMode::Conservation => "conservation",
            legionctl_api::BatteryMode::Rapid => "rapid",
        };
        if input.dry_run {
            return Ok(dry_run(&legionctl_machine::battery::mode_plan(
                &SysRoot::from_env(),
                mode,
            )?));
        }
        Ok(Changes {
            lines: self.system(async move |p| p.set_battery_mode(mode).await)?,
            dry_run: false,
        })
    }

    fn battery_usb_charging(&self, _: &Ctx, input: SwitchIn) -> Result<Changes> {
        let on = input.state == Switch::On;
        if input.dry_run {
            return Ok(dry_run(&legionctl_machine::battery::usb_charging_plan(
                &SysRoot::from_env(),
                on,
            )?));
        }
        Ok(Changes {
            lines: self.system(async move |p| p.set_usb_charging(on).await)?,
            dry_run: false,
        })
    }

    fn display_status(&self, _: &Ctx, _: Empty) -> Result<Lines> {
        let (monitors, antialiasing) = self
            .monitors()
            .context("no desktop session (Mutter) to ask")?;
        let on = |(offered, used): (bool, bool)| match (offered, used) {
            (false, _) => "not offered",
            (true, true) => "on",
            (true, false) => "off",
        };
        let mut lines: Vec<String> = monitors
            .iter()
            .map(|m| {
                format!(
                    "{} {}: {}x{} at {:.0} Hz (up to {:.0}), scale {}, {}, VRR {}, HDR {}",
                    m.connector,
                    m.name,
                    m.width,
                    m.height,
                    m.refresh,
                    m.max_refresh,
                    m.scale,
                    m.ppi
                        .map_or("density unknown".into(), |p| format!("{p:.0} PPI")),
                    on(m.vrr),
                    on(m.hdr)
                )
            })
            .collect();
        lines.push(format!("fonts: {antialiasing} antialiasing"));
        lines.extend(
            crate::display::findings(&SysRoot::from_env(), &home(), &monitors, &antialiasing)?
                .iter()
                .map(|f| format!("- {f}")),
        );
        Ok(Lines { lines })
    }

    fn display_fix(&self, ctx: &Ctx, input: DisplayFixIn) -> Result<Changes> {
        let all = input.finding == "all";
        let found: Vec<Finding> = self
            .display_findings()?
            .into_iter()
            .filter(|f| all || f.id == input.finding)
            .collect();
        let Some(first) = found.first() else {
            return Err(coded(
                Code::NotFound,
                format!(
                    "no finding {} now; legionctl display status and doctor show the findings with their ids",
                    input.finding
                ),
            ));
        };
        if !all && first.fixes.is_empty() {
            return Err(coded(
                Code::InvalidInput,
                format!(
                    "{} is a system change, not one of your own settings: {}",
                    first.id, first.text
                ),
            ));
        }
        let method = if input.persistent {
            crate::display::PERSISTENT
        } else {
            crate::display::TEMPORARY
        };
        let (fixes, system): (Vec<Finding>, Vec<Finding>) =
            found.into_iter().partition(|f| !f.fixes.is_empty());
        let fixes: Vec<Fix> = fixes.into_iter().flat_map(|f| f.fixes).collect();
        let mut changes = self.change(ctx, &fixes, method, input.dry_run)?;
        changes.lines.extend(
            system
                .iter()
                .map(|f| format!("not changed, a system change: {f}")),
        );
        Ok(changes)
    }

    fn boot_status(&self, _: &Ctx, _: Empty) -> Result<legionctl_api::BootState> {
        Ok(legionctl_machine::boot::state(&SysRoot::from_env())?.into_wire())
    }

    fn boot_install(&self, _: &Ctx, input: DryRunIn) -> Result<Changes> {
        self.boot_action("install", input.dry_run)
    }

    fn boot_rebuild(&self, _: &Ctx, input: DryRunIn) -> Result<Changes> {
        self.boot_action("rebuild", input.dry_run)
    }

    fn boot_remove(&self, _: &Ctx, input: DryRunIn) -> Result<Changes> {
        self.boot_action("remove", input.dry_run)
    }

    fn kernel_list(&self, _: &Ctx, _: Empty) -> Result<legionctl_api::Kernels> {
        Ok(legionctl_machine::kernel::list(&SysRoot::from_env())?.into_wire())
    }

    fn kernel_default(&self, _: &Ctx, input: legionctl_api::KernelIn) -> Result<Changes> {
        let name = input.name.clone();
        self.kernel_action(
            "default",
            input.name,
            String::new(),
            input.dry_run,
            |_, state| legionctl_machine::boot::default_plan(state, &name),
        )
    }

    fn kernel_try(&self, _: &Ctx, input: legionctl_api::KernelIn) -> Result<Changes> {
        let name = input.name.clone();
        self.kernel_action(
            "try",
            input.name,
            String::new(),
            input.dry_run,
            |_, state| legionctl_machine::boot::try_plan(state, &name),
        )
    }

    fn kernel_cmdline_set(&self, _: &Ctx, input: legionctl_api::KernelLineIn) -> Result<Changes> {
        let (name, line) = (input.name.clone(), input.line.clone());
        self.kernel_action(
            "cmdline",
            input.name,
            input.line,
            input.dry_run,
            |root, state| legionctl_machine::boot::cmdline_plan(root, state, &name, &line),
        )
    }

    fn kernel_cmdline_undo(&self, _: &Ctx, input: legionctl_api::KernelIn) -> Result<Changes> {
        let name = input.name.clone();
        self.kernel_action(
            "undo",
            input.name,
            String::new(),
            input.dry_run,
            |root, state| legionctl_machine::boot::undo_plan(root, state, &name),
        )
    }

    fn kernel_bench(
        &self,
        ctx: &Ctx,
        input: legionctl_api::KernelBenchIn,
    ) -> Result<legionctl_api::KernelBench> {
        self.bench_kernels(ctx, &input)
    }

    fn kernel_build(&self, _: &Ctx, input: legionctl_api::KernelBuildIn) -> Result<Changes> {
        self.build_kernel(&input)
    }

    fn greeter_status(&self, _: &Ctx, _: Empty) -> Result<legionctl_api::GreeterState> {
        Ok(legionctl_machine::greeter::state(&SysRoot::from_env())?.into_wire())
    }

    fn greeter_install(&self, _: &Ctx, input: DryRunIn) -> Result<Changes> {
        self.greeter_action("install", input.dry_run)
    }

    fn greeter_remove(&self, _: &Ctx, input: DryRunIn) -> Result<Changes> {
        self.greeter_action("remove", input.dry_run)
    }

    fn windows_status(&self, _: &Ctx, _: Empty) -> Result<WindowsState> {
        Ok(windows::state(&SysRoot::from_env())?.into_wire())
    }

    fn windows_reboot(&self, _: &Ctx, input: DryRunIn) -> Result<Changes> {
        if input.dry_run {
            let root = SysRoot::from_env();
            return Ok(dry_run(&windows::reboot_plan(
                &windows::state(&root)?,
                &legionctl_machine::boot::state(&root)?,
            )?));
        }
        Ok(Changes {
            lines: self.system(async |p| p.reboot_windows().await)?,
            dry_run: false,
        })
    }

    fn windows_vm_start(&self, _: &Ctx, _: Empty) -> Result<Lines> {
        let unit = format!("{}.service", legionctl_core::session::WINDOWS_UNIT);
        anyhow::ensure!(
            !unit_active(&unit)?,
            "the Windows VM runs already (legionctl windows vm status)"
        );
        // a run that crashed left the host changed: give it back first
        self.system(async |p| p.windows_vm_end().await)?;
        let dir = legionctl_core::session::windows_root();
        std::fs::create_dir_all(&dir)?;
        let state = dir.join("state");
        std::fs::write(&state, "starting")?;
        output(Command::new("systemctl").args(["--user", "start", "--no-block", &unit]))?;
        // the runner says how its start went (the polkit dialog waits for a human)
        let deadline = std::time::Instant::now() + std::time::Duration::from_mins(2);
        loop {
            // the runner replaces the file: a moment without it is no state yet
            let now = legionctl_machine::sysroot::read_file(&state)?.unwrap_or_default();
            if let Some(why) = now.strip_prefix("failed: ") {
                anyhow::bail!("the Windows VM did not start: {why}");
            }
            if now.starts_with("running") {
                return Ok(Lines {
                    lines: now.lines().map(String::from).collect(),
                });
            }
            anyhow::ensure!(
                std::time::Instant::now() < deadline,
                "the Windows VM did not start in 2 minutes; see journalctl --user -u {unit}"
            );
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }

    fn windows_vm_stop(&self, _: &Ctx, _: Empty) -> Result<Lines> {
        let unit = format!("{}.service", legionctl_core::session::WINDOWS_UNIT);
        let mut lines = Vec::new();
        if unit_active(&unit)? {
            // the unit's stop sends the power button and waits for Windows (legionctl windows-vm-stop)
            output(Command::new("systemctl").args(["--user", "stop", &unit]))?;
            lines.push("Windows shut down".into());
        }
        // the runner gives the host back when QEMU ends; after a crash, this does it
        lines.extend(self.system(async |p| p.windows_vm_end().await)?);
        Ok(Lines { lines })
    }

    fn windows_vm_autostart(&self, _: &Ctx, input: AutostartIn) -> Result<Lines> {
        let unit = format!("{}.service", legionctl_core::session::WINDOWS_UNIT);
        let on = input.mode != Autostart::Off;
        let mut lines = self.system(async move |p| p.set_windows_autostart(on).await)?;
        let verb = if on { "enable" } else { "disable" };
        output(Command::new("systemctl").args(["--user", verb, &unit]))?;
        lines.push(format!("{unit}: {verb}d"));
        // at boot only with lingering: the user's services start without a login (loginctl lets a user set it)
        if input.mode == Autostart::Boot {
            output(Command::new("loginctl").arg("enable-linger"))?;
            lines.push("your services start at boot (loginctl enable-linger)".into());
        }
        Ok(Lines { lines })
    }

    fn windows_vm_pause(&self, _: &Ctx, _: Empty) -> Result<Lines> {
        windows_qmp()?.execute("stop", None)?;
        Ok(Lines {
            lines: vec!["Windows: paused".into()],
        })
    }

    fn windows_vm_resume(&self, _: &Ctx, _: Empty) -> Result<Lines> {
        windows_qmp()?.execute("cont", None)?;
        Ok(Lines {
            lines: vec!["Windows: running again".into()],
        })
    }

    fn windows_disk(&self, _: &Ctx, input: legionctl_api::WindowsDiskIn) -> Result<Changes> {
        let attach = input.action == legionctl_api::DiskAction::Attach;
        // the plan refuses first, before the polkit dialog
        let steps = legionctl_machine::winvm::disk_plan(&SysRoot::from_env(), attach)?;
        if input.dry_run {
            return Ok(dry_run(&steps));
        }
        Ok(Changes {
            lines: self.system(async move |p| p.windows_disk(attach).await)?,
            dry_run: false,
        })
    }

    fn windows_vm_view(&self, _: &Ctx, _: Empty) -> Result<Lines> {
        let unit = format!("{}.service", legionctl_core::session::WINDOWS_UNIT);
        anyhow::ensure!(
            unit_active(&unit)?,
            "the Windows VM does not run (legionctl windows vm start)"
        );
        let dir = legionctl_core::session::windows_root();
        let spice = dir.join("spice.sock");
        // with the GPU, Windows draws on it: Looking Glass shows its frames; else the SPICE display
        let (viewer, args, shown) = if windows_has_gpu(&dir.join("qmp.sock"))? {
            let client = which("looking-glass-client").context(
                "Windows has the GPU, and looking-glass-client is not installed (package looking-glass; Windows needs \
                 the Looking Glass host)",
            )?;
            let file = legionctl_core::session::looking_glass_file();
            let args = vec![
                "-f".to_string(),
                file.to_string_lossy().into_owned(),
                "-c".into(),
                spice.to_string_lossy().into_owned(),
                "-p".into(),
                "0".into(),
            ];
            (client, args, format!("Looking Glass on {}", file.display()))
        } else {
            let client = which("remote-viewer")
                .context("remote-viewer is not installed: install the package virt-viewer")?;
            let uri = format!("spice+unix://{}", spice.display());
            (client, vec![uri.clone()], uri)
        };
        // a user service gets the desktop's environment (WAYLAND_DISPLAY), which the operator may not have
        output(
            Command::new("systemd-run")
                .args(["--user", "--collect", "--quiet", "--"])
                .arg(viewer)
                .args(&args),
        )?;
        Ok(Lines {
            lines: vec![format!("opened {shown}")],
        })
    }

    fn windows_vm_status(&self, _: &Ctx, _: Empty) -> Result<Lines> {
        let unit = format!("{}.service", legionctl_core::session::WINDOWS_UNIT);
        let dir = legionctl_core::session::windows_root();
        let state = legionctl_machine::sysroot::read_file(&dir.join("state"))?
            .unwrap_or_else(|| "never started".into());
        let mut lines = vec![if unit_active(&unit)? {
            "running".to_string()
        } else {
            format!(
                "not running (last: {})",
                state.lines().next().unwrap_or_default()
            )
        }];
        if unit_active(&unit)? {
            lines.extend(state.lines().skip(1).map(String::from));
        }
        Ok(Lines { lines })
    }

    fn internal_windows_vm_begin(
        &self,
        _: &Ctx,
        input: WindowsVmBeginIn,
    ) -> Result<WindowsVmBegun> {
        let usb = config(None)?.config.windows.vm.usb;
        let memlock = input.memlock_mib << 20;
        // with an overlay (a repair), Windows writes to it and the disk is only read
        let overlay = crate::wincare::start_overlay(&SysRoot::from_env())?;
        let (lines, devices, msdm) = if overlay.is_some() {
            self.system(async move |p| p.windows_overlay_begin(&usb).await)?
        } else {
            self.system(async move |p| p.windows_vm_begin(&usb, input.pid, memlock).await)?
        };
        Ok(WindowsVmBegun {
            lines,
            devices,
            msdm,
            overlay,
        })
    }

    fn windows_vm_overlay(&self, _: &Ctx, input: legionctl_api::WindowsOverlayIn) -> Result<Lines> {
        self.windows_overlay(input.action)
    }

    fn windows_vm_image(&self, _: &Ctx, input: legionctl_api::WindowsImageIn) -> Result<Lines> {
        self.windows_image(&input.path)
    }

    fn windows_vm_recovery(&self, _: &Ctx, _: Empty) -> Result<Lines> {
        crate::wincare::recovery()
    }

    fn windows_repair(&self, _: &Ctx, input: legionctl_api::WindowsRepairIn) -> Result<Lines> {
        crate::wincare::repair(input.step)
    }

    fn windows_restore_list(&self, _: &Ctx, _: Empty) -> Result<Lines> {
        crate::wincare::restore_list()
    }

    fn windows_restore_apply(
        &self,
        _: &Ctx,
        input: legionctl_api::WindowsRestoreIn,
    ) -> Result<Lines> {
        crate::wincare::restore_apply(input.point)
    }

    fn internal_windows_host(&self, _: &Ctx, input: legionctl_api::WindowsHostIn) -> Result<Lines> {
        self.windows_host(input.action)
    }

    fn internal_windows_vm_end(&self, _: &Ctx, _: Empty) -> Result<Lines> {
        Ok(Lines {
            lines: self.system(async |p| p.windows_vm_end().await)?,
        })
    }

    fn record(&self, _: &Ctx, input: RecordIn) -> Result<Recording> {
        let dir = input.dir.get()?;
        let empty = match std::fs::read_dir(dir) {
            Ok(mut d) => d.next().is_none(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => return Err(e).with_context(|| format!("listing {}", dir.display())),
        };
        anyhow::ensure!(
            empty,
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
            .collect::<Vec<_>>()
            .into_wire();
        // status reads more files than the decisions do; recording them lets every read operation replay
        legionctl_machine::snapshot::Snapshot::read(
            &plan.root,
            &plan.profile,
            Detail::Full,
            &config(None)?.config.load,
            throttle_logged(self),
        )?;
        let mut recording = plan.decisions(dir, events)?;
        let tree = dir.join("root");
        recording.paths = root.record_into(&tree)?;
        let layers = plan.layers.iter().map(|l| l.file.as_str());
        for file in [profile::DEFAULT_PATH, legionctl_machine::ledger::DB]
            .into_iter()
            .chain(layers)
        {
            // a file that is not there is not in the recording; one that cannot be read fails it (a gap would replay
            // other decisions)
            let bytes = match std::fs::read(root.path(file)) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e).with_context(|| format!("reading {file}")),
            };
            let to = SysRoot::new(&tree).path(file);
            std::fs::create_dir_all(to.parent().context("no parent")?)?;
            std::fs::write(to, bytes)?;
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
        let knobs = knobs::catalog(&Plan::load(None)?.machine())?
            .into_iter()
            .map(|k| (k.id, k.value))
            .collect();
        let runs = bench::Scenario::from(input.scenario)
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
        let mut failed = None;
        while failed.is_none() && runs.iter().any(|(_, r)| !r.is_finished()) {
            match self.sample(std::time::Duration::from_secs(1)) {
                Ok(s) => samples.push(s),
                Err(e) => failed = Some(e),
            }
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
        if let Some(e) = failed {
            return Err(e.context("a telemetry sample failed: the run has no summary"));
        }
        // the log keeps the domain value; the answer is its wire form (bench_run_of)
        let result = bench::BenchResult {
            scenario: input.scenario.into(),
            label: input.label,
            seconds: input.seconds,
            scores: scores.scores,
            problems: scores.problems,
            summary: bench::summarize(&samples),
            knobs,
            kernel: legionctl_machine::kernel::running_package(&SysRoot::from_env())?,
            release: Some(legionctl_machine::kernel::running_kernel(
                &SysRoot::from_env(),
            )?),
        };
        let db = Db::open_default()?;
        let id = db.record(&ctx.actor, "bench.result", &serde_json::to_value(&result)?)?;
        bench_run_of(db.event(id)?.context("the run was not stored")?)
    }

    fn tune(&self, _: &Ctx, input: TuneIn) -> Result<TuneReport> {
        let load = load::read_load(&SysRoot::from_env(), &config(None)?.config.load)?;
        anyhow::ensure!(
            input.force || load.level == load::Level::Idle,
            "the machine is not idle ({}); stop other work, or add --force",
            load.reasons.join("; ")
        );
        // the daemon has its own PATH: give it the programs' full paths
        let workloads = bench::Scenario::from(input.scenario)
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
            scenario: input.scenario.into(),
            seconds: input.seconds,
            cooldown_s: input.cooldown_s,
            workloads,
            per_kernel: input.per_kernel,
        })?;
        let report = self.system(async move |p| p.tune(&spec).await)?;
        Ok(serde_json::from_str::<legionctl_machine::tune::TuneReport>(&report)?.into_wire())
    }

    fn bench_list(&self, _: &Ctx, input: PageIn) -> Result<Page<BenchRun>> {
        let (after, limit) = page_of(input.cursor.as_deref(), input.limit)?;
        let filter = EventFilter {
            kinds: &["bench.result"],
            ..EventFilter::default()
        };
        let (rows, next) = Db::open_default()?.events_page(&filter, after, limit)?;
        Ok(Page {
            items: rows.into_iter().map(bench_run_of).collect::<Result<_>>()?,
            next: next.map(|n| n.to_string()),
        })
    }

    fn bench_compare(&self, _: &Ctx, input: BenchCompareIn) -> Result<BenchCompare> {
        let db = Db::open_default()?;
        let run = |id: i64| {
            db.event(id)?
                .filter(|e| e.kind == "bench.result")
                .with_context(|| format!("no bench run {id} (see legionctl bench list)"))
                .and_then(bench_run_of)
        };
        Ok(BenchCompare {
            a: run(input.earlier)?,
            b: run(input.later)?,
        })
    }

    fn knobs_list(&self, _: &Ctx, input: ProfileIn) -> Result<Knobs> {
        Ok(knobs::catalog(&Plan::load(path(input.profile.as_ref())?)?.machine())?.into_wire())
    }

    fn knobs_hold(&self, _: &Ctx, input: KnobHoldIn) -> Result<Changes> {
        let path = knob_path(input.profile.as_ref(), &input.knob)?;
        let values = std::collections::BTreeMap::from([(path, input.value)]);
        Ok(Changes {
            lines: self.system(async move |p| p.hold(values).await)?,
            dry_run: false,
        })
    }

    fn knobs_release(&self, _: &Ctx, input: KnobIn) -> Result<Changes> {
        let path = knob_path(input.profile.as_ref(), &input.knob)?;
        Ok(Changes {
            lines: self.system(async move |p| p.release(&path).await)?,
            dry_run: false,
        })
    }

    fn fan_status(&self, _: &Ctx, _: Empty) -> Result<FanState> {
        let state = self.system(async |p| p.fan_status().await)?;
        Ok(serde_json::from_str::<fan::FanState>(&state)?.into_wire())
    }

    fn fan_curve_set(&self, _: &Ctx, input: FanCurveIn) -> Result<Changes> {
        let levels = input.levels;
        if input.dry_run {
            return Ok(dry_run(&fan::set_curve(
                &Plan::load(None)?.machine(),
                &levels,
            )?));
        }
        // checked here too, so a wrong curve does not ask for a password first
        fan::validate(&levels).map_err(|e| coded(Code::InvalidInput, format!("{e:#}")))?;
        Ok(Changes {
            lines: self.system(async move |p| p.set_fan_curve(&levels).await)?,
            dry_run: false,
        })
    }

    fn fan_curve_reset(&self, _: &Ctx, input: DryRunIn) -> Result<Changes> {
        if input.dry_run {
            return Ok(dry_run(&fan::reset_curve(&Plan::load(None)?.machine())?));
        }
        Ok(Changes {
            lines: self.system(async |p| p.reset_fan_curve().await)?,
            dry_run: false,
        })
    }

    fn fan_max(&self, _: &Ctx, input: SwitchIn) -> Result<Changes> {
        let on = input.state == Switch::On;
        if input.dry_run {
            return Ok(dry_run(&fan::max(&Plan::load(None)?.machine(), on)?));
        }
        Ok(Changes {
            lines: self.system(async move |p| p.set_fan_max(on).await)?,
            dry_run: false,
        })
    }

    fn fan_clean(&self, _: &Ctx, input: SwitchIn) -> Result<Changes> {
        let on = input.state == Switch::On;
        if input.dry_run {
            return Ok(dry_run(&fan::clean(&Plan::load(None)?.machine(), on)?));
        }
        Ok(Changes {
            lines: self.system(async move |p| p.set_fan_full_speed(on).await)?,
            dry_run: false,
        })
    }

    fn fan_lock(&self, _: &Ctx, input: SwitchIn) -> Result<Changes> {
        let on = input.state == Switch::On;
        if input.dry_run {
            let plan = Plan::load(None)?;
            let mut steps = fan::lock_steps(on);
            steps.extend(fan::lock_refusal(&plan.machine()).map(policy::Step::Warn));
            return Ok(dry_run(&steps));
        }
        Ok(Changes {
            lines: self.system(async move |p| p.set_fan_lock(on).await)?,
            dry_run: false,
        })
    }

    fn undo(&self, ctx: &Ctx, input: UndoIn) -> Result<Changes> {
        let event = Db::open_default()?.event(input.event)?.ok_or_else(|| {
            coded(
                Code::NotFound,
                format!("no event {}; see legionctl events", input.event),
            )
        })?;
        // the user's own settings and files (`display fix`): no daemon, no polkit
        if event.kind == "display.change" {
            let fixes: Vec<Fix> = serde_json::from_value(event.payload["undo"].clone())?;
            let method = event.payload["method"]
                .as_u64()
                .and_then(|m| u32::try_from(m).ok())
                .context("the event has no Mutter method")?;
            return self.change(ctx, &fixes, method, false);
        }
        // a set switch changes the selection, not single values: holding its writes would not give the old selection
        if event.kind == "machine.switch-set" {
            return Err(coded(
                Code::InvalidInput,
                "a set switch is undone with: legionctl rules sets switch previous",
            ));
        }
        let values: std::collections::BTreeMap<String, String> = event.payload["lines"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| legionctl_machine::policy::parse_write(l.as_str()?))
            .filter(|(_, _, before)| !before.is_empty())
            .map(|(path, _, before)| (path, before))
            .collect();
        if values.is_empty() {
            return Err(coded(
                Code::InvalidInput,
                format!(
                    "event {} ({}) has no writes to turn back",
                    input.event, event.kind
                ),
            ));
        }
        Ok(Changes {
            lines: self.system(async move |p| p.hold(values).await)?,
            dry_run: false,
        })
    }

    fn plan(&self, _: &Ctx, _: Empty) -> Result<PlanView> {
        let plan = Plan::load(None)?;
        let m = plan.machine();
        let knobs = knobs::catalog(&m)?
            .into_iter()
            .filter(|k| k.desired.is_some())
            .collect::<Vec<_>>()
            .into_wire();
        // what the daemon writes at the next change of a layer or the workload: the Fn+Q choice stays
        let keep = Options {
            keep_override: true,
            ..Options::default()
        };
        Ok(PlanView {
            conditions: conditions(&m)?,
            knobs,
            changes: dry_run(&m.apply(None, keep)?.1).lines,
        })
    }

    fn rules_list(&self, _: &Ctx, _: Empty) -> Result<Rules> {
        let plan = Plan::load(None)?;
        let m = plan.machine();
        Ok(Rules {
            conditions: conditions(&m)?,
            layers: plan
                .layers
                .iter()
                .map(|l| rule_state(&m, l))
                .collect::<Result<_>>()?,
        })
    }

    fn rules_show(&self, _: &Ctx, input: RuleIn) -> Result<RuleDetail> {
        let plan = Plan::load(None)?;
        let m = plan.machine();
        let layer =
            configset::find(&plan.layers, &input.layer).map_err(|e| coded(Code::NotFound, e))?;
        Ok(RuleDetail {
            text: std::fs::read_to_string(plan.root.path(&layer.file))
                .with_context(|| format!("reading {}", layer.file))?,
            layer: rule_state(&m, layer)?,
        })
    }

    fn rules_enable(&self, _: &Ctx, input: RuleIn) -> Result<Changes> {
        Ok(Changes {
            lines: self.system(async move |p| p.set_layer_state(&input.layer, "on").await)?,
            dry_run: false,
        })
    }

    fn rules_disable(&self, _: &Ctx, input: RuleIn) -> Result<Changes> {
        Ok(Changes {
            lines: self.system(async move |p| p.set_layer_state(&input.layer, "off").await)?,
            dry_run: false,
        })
    }

    fn rules_edit(&self, _: &Ctx, input: RuleEditIn) -> Result<Changes> {
        let plan = Plan::load(None)?;
        let folder = if input.system {
            Folder::System
        } else {
            Folder::User
        };
        let layer = rules::edited(
            &plan.machine(),
            folder,
            &home(),
            &input.layer,
            &input.set,
            input.when.as_deref(),
        )
        .map_err(|e| coded(Code::InvalidInput, format!("{e:#}")))?;
        let mut lines = if input.system {
            let content = if layer.set.is_empty() {
                String::new()
            } else {
                configset::content(&layer)
            };
            self.system(async |p| p.set_layer(&layer.name, &content).await)?
        } else {
            let mut lines = vec![rules::write_own(&layer)?];
            lines.extend(self.system(async |p| p.reload().await)?);
            lines
        };
        // the rules between layers (a lock, the other power source) can still keep it out
        let now = Plan::load(None)?;
        if let Some(why) = now
            .layers
            .iter()
            .find(|l| l.folder == folder && l.name == layer.name && l.failed)
            .and_then(|l| l.off.as_ref())
        {
            lines.push(format!("warning: {} is not used: {why}", layer.file));
        }
        Ok(Changes {
            lines,
            dry_run: false,
        })
    }

    fn rules_reset(&self, _: &Ctx, input: RuleResetIn) -> Result<Changes> {
        let plan = Plan::load(None)?;
        let m = plan.machine();
        let knob = match &input.knob {
            Some(k) => Some(knobs::find(&m, k)?.ok_or_else(|| {
                coded(
                    Code::NotFound,
                    format!("no knob {k} on this machine (see legionctl knobs list)"),
                )
            })?),
            None => None,
        };
        // the layers to change: a knob's layers lose it, the rest are removed (a layer that sets nothing goes)
        let reset = |layer: &Layer| -> Option<Layer> {
            let mut layer = layer.clone();
            match &knob {
                Some(k) => {
                    layer.set.remove(&k.id)?;
                }
                None => layer.set.clear(),
            }
            Some(layer)
        };
        let mut lines = Vec::new();
        if input.system {
            for layer in plan
                .layers
                .iter()
                .filter(|l| l.folder == Folder::System)
                .filter_map(reset)
            {
                let content = if layer.set.is_empty() {
                    String::new()
                } else {
                    configset::content(&layer)
                };
                lines.extend(self.system(async |p| p.set_layer(&layer.name, &content).await)?);
            }
            if knob.is_none() {
                for layer in &plan.layers {
                    let id = layer.id();
                    if m.ledger.layer_enabled(&id)?.is_none() {
                        continue;
                    }
                    lines.extend(self.system(async |p| p.set_layer_state(&id, "default").await)?);
                }
            }
        } else {
            for layer in rules::own_layers(&plan.root, &plan.profile, &home())?
                .iter()
                .filter_map(reset)
            {
                lines.push(rules::write_own(&layer)?);
            }
            let held = m.ledger.holds()?.into_keys().filter(|path| {
                knob.as_ref()
                    .is_none_or(|k| *path == k.path || policy::held_by(&k.path, path))
            });
            for path in held {
                lines.extend(self.system(async |p| p.release(&path).await)?);
            }
            lines.extend(self.system(async |p| p.reload().await)?);
        }
        Ok(Changes {
            lines,
            dry_run: false,
        })
    }

    fn rules_sets_save(&self, _: &Ctx, input: legionctl_api::SetSaveIn) -> Result<Changes> {
        // refused before the polkit dialog
        legionctl_machine::sets::check_name(&input.name)
            .map_err(|e| coded(Code::InvalidInput, format!("{e:#}")))?;
        if input.boot {
            legionctl_machine::boot::takeover(&legionctl_machine::boot::state(
                &SysRoot::from_env(),
            )?)?;
        }
        Ok(Changes {
            lines: self.system(async |p| p.sets("save", &input.name, input.boot).await)?,
            dry_run: false,
        })
    }

    fn rules_sets_switch(&self, _: &Ctx, input: legionctl_api::SetSwitchIn) -> Result<Changes> {
        let ledger = Ledger::read(&SysRoot::from_env())?;
        let to = ledger.sets()?.remove(&input.name).ok_or_else(|| {
            coded(
                Code::NotFound,
                format!(
                    "no saved set {} (legionctl resources list shows the sets)",
                    input.name
                ),
            )
        })?;
        if input.dry_run {
            return Ok(Changes {
                lines: legionctl_machine::sets::changes(&ledger.selection()?, &to),
                dry_run: true,
            });
        }
        Ok(Changes {
            lines: self.system(async |p| p.sets("switch", &input.name, false).await)?,
            dry_run: false,
        })
    }

    fn rules_sets_remove(&self, _: &Ctx, input: legionctl_api::SetIn) -> Result<Changes> {
        Ok(Changes {
            lines: self.system(async |p| p.sets("remove", &input.name, false).await)?,
            dry_run: false,
        })
    }

    fn resources_list(&self, ctx: &Ctx, _: Empty) -> Result<legionctl_api::ResourceList> {
        crate::resources::list(self, ctx)
    }

    fn resources_act(
        &self,
        ctx: &Ctx,
        input: legionctl_api::ResourceActIn,
    ) -> Result<legionctl_api::Acted> {
        crate::resources::route(self, ctx, &input)
    }

    fn code_graph_refresh(&self, _: &Ctx, input: legionctl_api::GraphRefreshIn) -> Result<Lines> {
        crate::code::refresh(&input)
    }

    fn code_graph_refs(&self, _: &Ctx, input: RepoIn) -> Result<legionctl_api::GraphRefs> {
        crate::code::refs(&input)
    }

    fn code_graph_query(
        &self,
        _: &Ctx,
        input: legionctl_api::GraphQueryIn,
    ) -> Result<legionctl_api::CodeNodes> {
        crate::code::query(&input)
    }

    fn code_graph_impact(
        &self,
        _: &Ctx,
        input: legionctl_api::GraphImpactIn,
    ) -> Result<legionctl_api::CodeNodes> {
        crate::code::impact(&input)
    }

    fn code_graph_tree(
        &self,
        _: &Ctx,
        input: legionctl_api::GraphTreeIn,
    ) -> Result<legionctl_api::CodeTree> {
        Ok(legionctl_core::codegraph::tree(
            input.repo.get()?,
            &input.prefix,
            &input.rev,
            input.build,
        )?
        .into_wire())
    }

    fn code_graph_changes(
        &self,
        _: &Ctx,
        input: legionctl_api::GraphChangesIn,
    ) -> Result<Vec<legionctl_api::ChangeSet>> {
        crate::code::changes(&input)
    }

    fn code_graph_annotate(&self, ctx: &Ctx, input: legionctl_api::AnnotateIn) -> Result<Lines> {
        crate::code::annotate(ctx, &input)
    }

    fn code_graph_annotations(
        &self,
        _: &Ctx,
        input: legionctl_api::AnnotationsIn,
    ) -> Result<Vec<legionctl_api::Annotation>> {
        crate::code::annotations(&input)
    }

    fn code_graph_prune(&self, _: &Ctx, input: legionctl_api::GraphPruneIn) -> Result<Lines> {
        crate::code::prune(&input)
    }

    fn code_graph_hook(&self, _: &Ctx, input: legionctl_api::GraphHookIn) -> Result<Lines> {
        crate::code::hook(&input)
    }

    fn code_plan_check(
        &self,
        _: &Ctx,
        input: legionctl_api::PlanCheckIn,
    ) -> Result<legionctl_api::PlanCheck> {
        crate::code::plan_check(&input)
    }

    fn code_plan_status(
        &self,
        _: &Ctx,
        input: legionctl_api::PlanStatusIn,
    ) -> Result<legionctl_api::PlanStatus> {
        crate::code::plan_status(&input)
    }

    fn thread_start(&self, _: &Ctx, input: legionctl_api::ThreadStartIn) -> Result<Lines> {
        crate::thread::start(&input)
    }

    fn thread_repo(&self, _: &Ctx, input: legionctl_api::ThreadRepoIn) -> Result<Lines> {
        let root = config(Some(&input.repo))?.config.workspace.root;
        Ok(Lines {
            lines: dossiers()?.thread_repo(
                input.thread,
                input.repo.get()?,
                input.base.as_deref(),
                &root,
            )?,
        })
    }

    fn thread_list(&self, _: &Ctx, _: Empty) -> Result<Vec<legionctl_api::ThreadInfo>> {
        crate::thread::list()
    }

    fn thread_show(
        &self,
        _: &Ctx,
        input: legionctl_api::ThreadIdIn,
    ) -> Result<legionctl_api::ThreadView> {
        crate::thread::show(&input)
    }

    fn thread_import(&self, _: &Ctx, input: legionctl_api::ThreadImportIn) -> Result<Lines> {
        let copy = dossiers()?.thread_import(input.thread, &input.account)?;
        Ok(Lines {
            lines: vec![format!(
                "thread {} imported to account {} as thread {copy}; thread {} is read-only now",
                input.thread, input.account, input.thread
            )],
        })
    }

    fn ops_list(
        &self,
        _: &Ctx,
        input: legionctl_api::OpsListIn,
    ) -> Result<Vec<legionctl_api::OpEntry>> {
        Ok(legionctl_api::OPS
            .iter()
            .filter(|op| !op.internal())
            .map(|op| legionctl_api::OpEntry {
                name: op.name.to_string(),
                group: op.name.split('.').next().unwrap_or(op.name).to_string(),
                class: op.class,
                summary: op.summary().to_string(),
                dry_run: (op.input)()["properties"].get("dry_run").is_some(),
                fields: u32::try_from(legionctl_api::form::fields(op).len()).unwrap_or(u32::MAX),
            })
            .filter(|e| input.group.as_ref().is_none_or(|g| *g == e.group))
            .collect())
    }

    fn facts_set(&self, ctx: &Ctx, input: legionctl_api::FactSetIn) -> Result<Lines> {
        crate::facts::set(ctx, &input)
    }

    fn facts_at(
        &self,
        _: &Ctx,
        input: legionctl_api::FactsAtIn,
    ) -> Result<Vec<legionctl_api::Fact>> {
        crate::facts::at_op(&input)
    }

    fn facts_history(
        &self,
        _: &Ctx,
        input: legionctl_api::FactsHistoryIn,
    ) -> Result<Vec<legionctl_api::Fact>> {
        crate::facts::history(&input)
    }

    fn facts_diff(
        &self,
        _: &Ctx,
        input: legionctl_api::FactsDiffIn,
    ) -> Result<Vec<legionctl_api::FactChange>> {
        crate::facts::diff(&input)
    }

    fn facts_stale(
        &self,
        _: &Ctx,
        input: legionctl_api::FactsAtIn,
    ) -> Result<Vec<legionctl_api::Fact>> {
        crate::facts::stale(&input)
    }

    fn views_list(
        &self,
        _: &Ctx,
        input: legionctl_api::ViewsListIn,
    ) -> Result<Vec<legionctl_api::ViewInfo>> {
        crate::views::list(&input)
    }

    fn views_check(&self, _: &Ctx, input: legionctl_api::ViewTextIn) -> Result<Lines> {
        crate::views::check_text(&input)
    }

    fn views_save(&self, ctx: &Ctx, input: legionctl_api::ViewSaveIn) -> Result<Lines> {
        crate::views::save(ctx, &input)
    }

    fn ask(&self, ctx: &Ctx, input: legionctl_api::AskIn) -> Result<legionctl_api::ViewAnswer> {
        crate::views::ask(self, ctx, &input)
    }

    fn views_version(&self, ctx: &Ctx, input: legionctl_api::ViewVersionIn) -> Result<Lines> {
        crate::views::version_op(ctx, &input)
    }

    fn views_render(
        &self,
        ctx: &Ctx,
        input: legionctl_api::ViewRenderIn,
    ) -> Result<legionctl_api::ViewData> {
        crate::views::render(self, ctx, &input)
    }

    fn views_place(&self, _: &Ctx, input: legionctl_api::ViewPlaceIn) -> Result<Lines> {
        crate::views::place(&input)
    }

    fn views_parts(
        &self,
        _: &Ctx,
        input: legionctl_api::ViewPartsIn,
    ) -> Result<Vec<legionctl_api::PartInfo>> {
        crate::views::parts(&input)
    }

    fn views_cost(&self, _: &Ctx, _: Empty) -> Result<Vec<legionctl_api::AskCost>> {
        crate::views::cost()
    }

    fn thread_agent(&self, _: &Ctx, input: legionctl_api::ThreadAgentIn) -> Result<Lines> {
        crate::thread::agent(&input)
    }

    fn thread_account(&self, _: &Ctx, input: legionctl_api::ThreadAccountIn) -> Result<Lines> {
        crate::thread::account(&input)
    }

    fn thread_join(&self, _: &Ctx, input: legionctl_api::ThreadJoinIn) -> Result<Lines> {
        crate::thread::join(&input)
    }

    fn thread_leave(&self, _: &Ctx, input: legionctl_api::ThreadMemberIn) -> Result<Lines> {
        crate::thread::leave(&input)
    }

    fn thread_send(&self, _: &Ctx, input: legionctl_api::ThreadSendIn) -> Result<Lines> {
        crate::thread::send(&input)
    }

    fn thread_inbox(&self, _: &Ctx, input: SessionIn) -> Result<Vec<legionctl_api::ThreadMessage>> {
        crate::thread::inbox(&input)
    }

    fn thread_ack(&self, _: &Ctx, input: legionctl_api::ThreadAckIn) -> Result<Lines> {
        crate::thread::ack(&input)
    }

    fn thread_declare(&self, _: &Ctx, input: legionctl_api::ThreadDeclareIn) -> Result<Lines> {
        crate::thread::declare(&input)
    }

    fn thread_pace(&self, _: &Ctx, input: legionctl_api::ThreadPaceIn) -> Result<Lines> {
        crate::thread::pace(&input)
    }

    fn thread_rules_list(
        &self,
        _: &Ctx,
        input: legionctl_api::ThreadIdIn,
    ) -> Result<Vec<legionctl_api::WatchRule>> {
        crate::thread::rules(&input)
    }

    fn thread_rules_add(&self, _: &Ctx, input: legionctl_api::RuleAddIn) -> Result<Lines> {
        crate::thread::rule_add(&input)
    }

    fn thread_rules_rm(&self, _: &Ctx, input: legionctl_api::RuleRmIn) -> Result<Lines> {
        crate::thread::rule_rm(&input)
    }

    fn thread_peers(
        &self,
        _: &Ctx,
        input: legionctl_api::PeersIn,
    ) -> Result<Vec<legionctl_api::Peer>> {
        crate::thread::peers(&input)
    }

    fn thread_observe(
        &self,
        _: &Ctx,
        input: legionctl_api::ObserveIn,
    ) -> Result<legionctl_api::Observed> {
        crate::thread::observe(&input)
    }

    fn agent_tool(&self, _: &Ctx, input: legionctl_api::AgentToolIn) -> Result<HookReply> {
        crate::thread::tool(&input)
    }

    fn thread_gate(&self, _: &Ctx, input: legionctl_api::ThreadGateIn) -> Result<Guard> {
        crate::thread::gate(&input)
    }

    fn thread_branch(&self, _: &Ctx, input: legionctl_api::ThreadBranchIn) -> Result<Lines> {
        crate::thread::branch(&input)
    }

    fn thread_compensate(
        &self,
        _: &Ctx,
        input: legionctl_api::ThreadCompensateIn,
    ) -> Result<Lines> {
        crate::thread::compensate(&input)
    }

    fn thread_teammate(&self, _: &Ctx, input: legionctl_api::TeammateIn) -> Result<Guard> {
        crate::thread::teammate(&input)
    }

    fn rlcd_ask(
        &self,
        _: &Ctx,
        input: legionctl_api::RlcdAskIn,
    ) -> Result<legionctl_api::RlcdDecision> {
        Ok(crate::rlcd::ask_op(&self.runtime, &dossiers()?, &input)?.into_wire())
    }

    fn rlcd_decisions(
        &self,
        _: &Ctx,
        input: legionctl_api::RlcdDecisionsIn,
    ) -> Result<Vec<legionctl_api::RlcdDecision>> {
        Ok(dossiers()?.decisions(input.limit, None)?.into_wire())
    }

    fn knobs_show(&self, _: &Ctx, input: InspectIn) -> Result<KnobDetail> {
        let plan = Plan::load(path(input.profile.as_ref())?)?;
        let knob = knobs::find(&plan.machine(), &input.knob)?.with_context(|| {
            format!(
                "no knob {} on this machine (see legionctl knobs)",
                input.knob
            )
        })?;
        let (after, limit) = page_of(input.cursor.as_deref(), input.limit)?;
        let filter = EventFilter {
            naming: Some(&knob.path),
            ..EventFilter::default()
        };
        let history = page(Db::open_default()?.events_page(&filter, after, limit)?);
        Ok(KnobDetail {
            knob: knob.into_wire(),
            history,
        })
    }

    fn session_start(&self, _: &Ctx, input: SessionStartIn) -> Result<Launch> {
        let dir = input.repo.get()?;
        let resolved = config(Some(&input.repo))?;
        let store = Dossiers::open(&resolved.config.dossier)?;
        let dossier = match input.dossier {
            Some(d) => d,
            None => work_dossier(&store, dir)?,
        };
        let token = legionctl_api::random_token()?;
        let start = Start {
            agent: &input.agent,
            dossier: &dossier,
            steal: input.steal,
            token: &token,
            dir,
            topic: None,
            headless: false,
            account: input.account.as_deref(),
            inside: input.inside.into(),
            gpu: input.gpu,
            member: None,
            capabilities: &main_agent_sets(&resolved.config, &input.capability),
            parent: None,
            tools: &[],
        };
        let launch = store.start_session(&start, &resolved.config)?;
        Db::open_default()?.issue_token(&launch.id, &token)?;
        Ok(launch.into_wire())
    }

    fn session_run(&self, _: &Ctx, input: SessionRunIn) -> Result<RunOutput> {
        let resolved = config(Some(&input.repo))?;
        let store = Dossiers::open(&resolved.config.dossier)?;
        // the user's thread: its dossier, its folder and its ACP session; the prompt and the answer go into its log
        let thread = match input.thread {
            Some(id) => {
                let info = store
                    .threads()?
                    .into_iter()
                    .find(|t| t.id == id)
                    .with_context(|| format!("no thread {id}"))?;
                if let Some(owner) = &info.owner {
                    anyhow::bail!(
                        "thread {id} is the thread of session {owner}: a run goes into a thread of the user (a chat; \
                         the app's /chat starts one)"
                    );
                }
                anyhow::ensure!(
                    input.dossier.as_ref().is_none_or(|d| *d == info.dossier),
                    "thread {id} works on dossier {}, not {}",
                    info.dossier,
                    input.dossier.as_deref().unwrap_or_default()
                );
                let root = &resolved.config.workspace.root;
                Some((info, store.thread_folder(id, root)?, store.thread_acp(id)?))
            }
            None => None,
        };
        let dir = match &thread {
            Some((_, folder, _)) => folder.as_path(),
            None => input.repo.get()?,
        };
        let dossier = match (&thread, input.dossier.clone()) {
            (Some((info, _, _)), _) => info.dossier.clone(),
            (None, Some(d)) => d,
            (None, None) => work_dossier(&store, dir)?,
        };
        let acp = input
            .acp
            .clone()
            .or_else(|| thread.as_ref().and_then(|(_, _, acp)| acp.clone()));
        let tag = input
            .tag
            .clone()
            .or_else(|| input.thread.map(|id| format!("thread:{id}")));
        let prompt = match input.thread {
            Some(id) if acp.is_none() => conversation_prompt(&store, id, dir, &input.prompt)?,
            _ => input.prompt.clone(),
        };
        if let Some(id) = input.thread {
            store.thread_note(
                id,
                "user",
                "thread.say",
                &serde_json::json!({"text": input.prompt, "agent": input.agent}),
            )?;
        }
        // a thread on a repository works in its own worktree (topic thread-<id>)
        let topic = thread
            .as_ref()
            .and_then(|(info, _, _)| info.branch.as_ref().map(|_| format!("thread-{}", info.id)));
        let out = self.run_headless(
            &store,
            &resolved,
            (&input.agent, &dossier, dir, topic.as_deref()),
            &input,
            (tag, acp, &prompt),
        );
        if let Some(id) = input.thread {
            let row = match &out {
                Ok(run) => (
                    "thread.answer",
                    serde_json::json!({"text": run.text, "acp": run.acp, "session": run.id, "ms": run.ms}),
                ),
                Err(e) => (
                    "thread.fail",
                    serde_json::json!({"error": format!("{e:#}")}),
                ),
            };
            store.thread_note(id, &input.agent, row.0, &row.1)?;
        }
        out
    }

    fn session_list(&self, _: &Ctx, _: Empty) -> Result<Sessions> {
        Ok(dossiers()?
            .sessions()?
            .into_iter()
            .map(|session| {
                // one session whose state cannot be read shows why; the others are still listed
                let state = legionctl_core::session::state(&session.id, session.inside)
                    .unwrap_or_else(|e| format!("unknown: {e:#}"));
                SessionState {
                    state,
                    session: session.into_wire(),
                }
            })
            .collect())
    }

    fn session_commands(
        &self,
        _: &Ctx,
        input: SessionIdIn,
    ) -> Result<Vec<legionctl_api::AgentCommand>> {
        anyhow::ensure!(
            dossiers()?.sessions()?.iter().any(|s| s.id == input.id),
            "no session {} (session list)",
            input.id
        );
        let newest = Db::open_default()?.last_event(&EventFilter {
            kinds: &["session.commands"],
            field: Some(("$.session", &input.id)),
            naming: None,
        })?;
        Ok(match newest {
            Some(event) => serde_json::from_value(event.payload["commands"].clone())?,
            None => Vec::new(),
        })
    }

    fn session_stop(&self, _: &Ctx, input: SessionIdIn) -> Result<Lines> {
        session_control(legionctl_core::session::Control::Stop, &input.id, "stopped")
    }

    fn session_activity(&self, _: &Ctx, input: SessionActivityIn) -> Result<Page<SessionExec>> {
        let (after, limit) = page_of(input.cursor.as_deref(), input.limit)?;
        let filter = EventFilter {
            kinds: &["session.exec", "session.open"],
            field: Some(("$.session", &input.id)),
            naming: None,
        };
        let (rows, next) = Db::open_default()?.events_page(&filter, after, limit)?;
        Ok(Page {
            items: rows
                .into_iter()
                .filter_map(|e| {
                    Some(SessionExec {
                        at: e.ts,
                        pid: u32::try_from(e.payload["pid"].as_u64()?).ok()?,
                        argv: serde_json::from_value(e.payload["argv"].clone()).unwrap_or_default(),
                        path: e.payload["path"].as_str().map(String::from),
                    })
                })
                .collect(),
            next: next.map(|n| n.to_string()),
        })
    }

    fn session_pause(&self, _: &Ctx, input: SessionIdIn) -> Result<Lines> {
        session_control(legionctl_core::session::Control::Pause, &input.id, "paused")
    }

    fn session_unpause(&self, _: &Ctx, input: SessionIdIn) -> Result<Lines> {
        session_control(
            legionctl_core::session::Control::Unpause,
            &input.id,
            "running again",
        )
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
            (false, &[]),
        )?;
        Db::open_default()?.issue_token(&launch.id, &token)?;
        Ok(launch.into_wire())
    }

    fn gates_check(&self, _: &Ctx, input: GateIn) -> Result<Guard> {
        let dir = input.project.get()?;
        let deny = config(Some(&input.project))?.config.gates.git.deny;
        // an alias is resolved by the real git outside the sandbox, so the agent cannot hide a denied command
        // git config --get: 0 found, 1 not set, another code an error
        let is_alias = |sub: &str| -> Result<bool> {
            Ok(
                legionctl_core::git::found(dir, &["config", "--get", &format!("alias.{sub}")])?
                    .is_some(),
            )
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
        Ok(config(Some(&input.project))?
            .explain(&input.key)
            .into_wire())
    }

    fn config_settings(&self, _: &Ctx, input: ProjectIn) -> Result<legionctl_api::SettingsView> {
        let resolved = config(Some(&input.project))?;
        let items = legionctl_core::settings::registry()?
            .into_iter()
            .map(|setting| {
                let explained = resolved.explain(&setting.key);
                let from = explained
                    .settings
                    .last()
                    .map_or_else(|| "defaults".to_string(), |(_, s)| s.source.layer.clone());
                legionctl_api::SettingRow {
                    setting,
                    value: explained.value,
                    from,
                }
            })
            .collect();
        Ok(legionctl_api::SettingsView {
            items,
            problems: resolved.problems,
        })
    }

    fn config_layers(&self, _: &Ctx, input: ProjectIn) -> Result<LayerList> {
        let resolved = config(Some(&input.project))?;
        Ok(LayerList {
            layers: resolved.layers.into_wire(),
            locked: resolved.config.config.locked,
            problems: resolved.problems,
        })
    }
}

impl Operator {
    /// A headless run of `agent` on `dossier` in `dir`, with the output `tag` and the ACP session `acp` to go on in.
    fn run_headless(
        &self,
        store: &Dossiers,
        resolved: &Resolved,
        (agent, dossier, dir, topic): (&str, &str, &Path, Option<&str>),
        input: &SessionRunIn,
        (tag, acp, prompt): (Option<String>, Option<String>, &str),
    ) -> Result<RunOutput> {
        let token = legionctl_api::random_token()?;
        let start = Start {
            agent,
            dossier,
            steal: input.steal,
            token: &token,
            dir,
            topic,
            headless: true,
            account: input.account.as_deref(),
            inside: input.inside.into(),
            gpu: input.gpu,
            member: None,
            capabilities: &main_agent_sets(&resolved.config, &input.capability),
            parent: None,
            tools: &input.tools,
        };
        for tool in &input.tools {
            legionctl_api::OPS
                .iter()
                .find(|op| op.name == tool && !op.internal())
                .with_context(|| format!("no operation {tool} to bind the agent to"))?;
        }
        let launch = store.start_session(&start, &resolved.config)?;
        let db = Db::open_default()?;
        db.issue_token(&launch.id, &token)?;
        // `load`: the caller asked to go on in `acp`, so an agent that cannot load it fails the run; `tools`: a resume
        // binds the run again
        let job_input = serde_json::json!({"session": launch.id, "prompt": prompt, "tag": tag,
            "acp": acp, "load": acp.is_some(), "model": input.model, "tools": input.tools});
        let job = db.job_begin("session.run", &job_input)?;
        let out = self.runtime.block_on(crate::acp::run(
            launch,
            prompt,
            (job, &job_input),
            stoppable(job),
            store,
        ));
        STOPS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&job);
        end_job(&db, job, &out)?;
        out
    }
}
