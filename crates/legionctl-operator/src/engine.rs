//! The operations of `legionctl-api`, carried out as the user. Machine state is read here; machine changes go to
//! `legionctl-system` over D-Bus, which asks polkit. Every call that is not a read is recorded in the event log.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use legionctl_api::{
    Account, BenchCompare, BenchCompareIn, BenchIn, BenchRun, Code, DossierAddIn, DossierAppendIn,
    DossierCheckIn, DossierEditIn, DossierFindings, DossierList, DossierReadIn, DossierSearchIn,
    DossierUseIn, DossierWriteIn, DryRunIn, Events, EventsIn, GateIn, Guard, GuardIn, HookReply,
    HookRunIn, InspectIn, IntoWire, KnobDetail, Knobs, Launch, Lines, Locks, MonitorIn, Readings,
    RecordIn, Recording, ReplayIn, Samples, SensorsIn, SessionExec, SessionId, SessionIdIn,
    SessionIn, SessionResumeIn, SessionStartIn, SessionState, Sessions, ToolCall, ToolCallRow,
    ToolCallsIn, TuneIn, TuneReport, WindowsState, coded,
};
use legionctl_api::{
    AccountAddIn, CapabilityList, Changes, CheckpointIn, CleanupIn, ClientPath, CloseIn,
    ConfigFiles, ConfigView, Ctx, Empty, Engine, ExplainIn, Explanation, Findings, GpuPrimaryIn,
    GpuState, HookAddIn, HookBindIn, HookTable, IdIn, LayerList, LoadState, PowerApplyIn,
    PowerSource, ProfileIn, ProjectIn, ReminderAddIn, Reminders, RunEnv, RunEnvIn, SetupApplyIn,
    SetupReport, SkillList, Snapshot, StatusIn, Validation, Workspace, WorkspaceIn,
    WorkspaceOpenIn, WorkspaceStatus, Workspaces,
};
use legionctl_api::{
    Autostart, AutostartIn, DisplayFixIn, FanCurveIn, FanState, GpuGiveIn, GpuName, GpuOwner,
    KnobHoldIn, KnobIn, PackIn, PackKind, Switch, SwitchIn, UndoIn, VfioBindIn, VfioDevices,
    VfioIn, WindowsVmBeginIn, WindowsVmBegun,
};
use legionctl_api::{
    CachePurgeIn, CacheStat, CachedCall, EventRow, JobIdIn, Page, PageIn, Principal,
    SessionActivityIn,
};
use legionctl_core::cache::Cache;
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
use legionctl_machine::{bench, cleanup, fan, gpu, knobs, load, windows};

pub struct Operator {
    /// Runs the D-Bus calls of machine changes; engine methods run on blocking threads.
    pub runtime: tokio::runtime::Handle,
    /// The telemetry ring: the last [`RING`] samples of `monitor`, `sensors` and `/v1/metrics`, oldest first.
    pub ring: std::sync::Mutex<std::collections::VecDeque<telemetry::Sample>>,
}

/// Samples the telemetry ring keeps (10 minutes of `sensors --watch`).
const RING: usize = 600;

fn has_command(name: &str) -> bool {
    which(name).is_some()
}

fn ppd_current() -> Option<String> {
    let out = Command::new("powerprofilesctl").arg("get").output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
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

/// The stop signals of the headless runs in progress, by job: the session policy offloads a run with it.
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
fn end_job<T: serde::Serialize>(db: &Db, job: i64, out: &Result<T>) -> Result<()> {
    let outcome = match out {
        Ok(value) => Ok(serde_json::to_value(value)?),
        Err(e) => Err(format!("{e:#}")),
    };
    db.job_end(job, &outcome)
}

/// The path of the knob `knob` (an id or a path) on this machine.
fn knob_path(profile: Option<&ClientPath>, knob: &str) -> Result<String> {
    let plan = Plan::load(path(profile)?)?;
    Ok(knobs::find(&plan.machine(), knob)
        .ok_or_else(|| {
            coded(
                Code::NotFound,
                format!("no knob {knob} on this machine (see legionctl knobs list)"),
            )
        })?
        .path)
}

/// The conditions the layers are tested against now.
fn conditions(m: &Machine) -> Conditions {
    let c = m.conditions(
        m.source(),
        m.ledger
            .get(legionctl_machine::ledger::Key::Override)
            .as_deref(),
    );
    Conditions {
        power: c.power.name().into(),
        fnq: c.fnq,
        workload: c.workload.into_iter().collect(),
        kernel: c.kernel,
    }
}

/// A layer with its id and whether it applies now.
fn rule_state(m: &Machine, layer: &Layer) -> RuleState {
    let c = m.conditions(
        m.source(),
        m.ledger
            .get(legionctl_machine::ledger::Key::Override)
            .as_deref(),
    );
    RuleState {
        id: layer.id(),
        active: layer.off.is_none() && layer.applies(&c),
        layer: layer.clone().into_wire(),
    }
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
            let (project, tag) = legionctl_core::images::project(&repo);
            legionctl_core::images::image(&project, &name, &tag)
        }
        None => container.base.clone(),
    };
    let context = legionctl_core::cache::default_path()
        .with_file_name("pack")
        .join(legionctl_core::images::project(&repo).0);
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
fn output(command: &mut Command) -> Result<Vec<u8>> {
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
pub(crate) fn windows_qmp() -> Result<legionctl_core::qmp::Qmp> {
    legionctl_core::qmp::Qmp::connect(&legionctl_core::session::windows_root().join("qmp.sock"))
        .context("no Windows VM runs (legionctl windows vm start)")
}

/// The NVIDIA GPU's IOMMU group and who has it: Linux (its driver), Windows (on vfio-pci and in the running VM), or
/// no one (on vfio-pci).
pub(crate) fn nvidia_owner(root: &SysRoot) -> Result<(Vec<String>, GpuOwner)> {
    let group = legionctl_machine::vfio::nvidia_group(root)?;
    let in_vm = windows_has_gpu(&legionctl_core::session::windows_root().join("qmp.sock"));
    let owner = match legionctl_machine::vfio::driver(root, &group[0]).as_deref() {
        Some("vfio-pci") if in_vm => GpuOwner::Windows,
        Some("vfio-pci") => GpuOwner::None,
        _ => GpuOwner::Linux,
    };
    Ok((group, owner))
}

/// Whether the running Windows VM (QMP at `socket`) has the GPU (`gpu give nvidia windows` added `gpu0`).
fn windows_has_gpu(socket: &Path) -> bool {
    legionctl_core::qmp::Qmp::connect(socket)
        .and_then(|mut q| {
            q.execute(
                "qom-list",
                Some(serde_json::json!({"path": "/machine/peripheral"})),
            )
        })
        .is_ok_and(|ids| {
            ids.as_array()
                .is_some_and(|ids| ids.iter().any(|i| i["name"] == "gpu0"))
        })
}

/// Whether user unit `unit` is active.
pub(crate) fn unit_active(unit: &str) -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "-q", unit])
        .status()
        .is_ok_and(|s| s.success())
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

/// The machine, its profile, the Fn+Q ledger and the configset layers, as the planners need them.
struct Plan {
    root: SysRoot,
    profile: Profile,
    ledger: Ledger,
    layers: Vec<Layer>,
}

impl Plan {
    fn load(profile: Option<&Path>) -> Result<Plan> {
        Plan::at(SysRoot::from_env(), profile)
    }

    /// The machine under `root`; without `profile`, the profile installed there.
    fn at(root: SysRoot, profile: Option<&Path>) -> Result<Plan> {
        let installed = root.path(profile::DEFAULT_PATH);
        let ledger = Ledger::open(&root)?;
        let profile = profile::load(Some(profile.unwrap_or(&installed)))?.0;
        let layers = configset::load(&root, &profile, &ledger);
        Ok(Plan {
            root,
            profile,
            ledger,
            layers,
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
            knobs: knobs::catalog(&machine).into_wire(),
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
            crate::display::shown(&SysRoot::from_env(), &state),
            antialiasing,
        ))
    }

    /// The graphics findings, and with a desktop session (Mutter on the session bus) the monitors' findings.
    fn display_findings(&self) -> Vec<Finding> {
        let (root, home) = (SysRoot::from_env(), home());
        let mut out = legionctl_machine::graphics::findings(&root, &home);
        if let Ok((monitors, antialiasing)) = self.monitors() {
            out.extend(crate::display::findings(
                &root,
                &home,
                &monitors,
                &antialiasing,
            ));
        }
        out
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

    /// A boot action: its plan for a dry run, else the daemon carries it out.
    fn boot_action(
        &self,
        action: &'static str,
        dry: bool,
        plan: impl FnOnce(&SysRoot) -> Result<Vec<legionctl_machine::policy::Step>>,
    ) -> Result<Changes> {
        if dry {
            return Ok(dry_run(&plan(&SysRoot::from_env())?));
        }
        Ok(Changes {
            lines: self.system(async move |p| p.boot(action).await)?,
            dry_run: false,
        })
    }

    /// A greeter action: its plan for a dry run, else the daemon carries it out (the plan refuses first, before the
    /// polkit dialog).
    fn greeter_action(
        &self,
        action: &'static str,
        dry: bool,
        plan: fn(&SysRoot) -> Result<Vec<legionctl_machine::policy::Step>>,
    ) -> Result<Changes> {
        let steps = plan(&SysRoot::from_env())?;
        if dry {
            return Ok(dry_run(&steps));
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
            actions: hooks::actions(config).into_wire(),
            bindings: hooks::bindings(config).into_wire(),
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
    if Event::from(input.event) == Event::Prompt
        && config.dossier.gate
        && has_session
        && active.is_none()
        && !runs_dossier
    {
        return gate_message(resolved).map(Some);
    }
    if Event::from(input.event) != Event::Stop {
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
pub(crate) fn config(project: Option<&ClientPath>) -> Result<Resolved> {
    resolve(&Dirs::from_env(), path(project)?)
}

impl Operator {
    /// One telemetry sample over `window`; the root-only part comes from `legionctl-system`, and stays unknown when
    /// the daemon does not answer.
    pub fn sample(&self, window: std::time::Duration) -> telemetry::Sample {
        let root_part = || {
            self.runtime
                .block_on(async {
                    let conn = zbus::Connection::system().await?;
                    let json = System1Proxy::new(&conn).await?.sample().await?;
                    anyhow::Ok(serde_json::from_str::<RootSample>(&json)?)
                })
                .unwrap_or_default()
        };
        let sample = telemetry::sample(&SysRoot::from_env(), window, &root_part);
        let mut ring = self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if ring.len() == RING {
            ring.pop_front();
        }
        ring.push_back(sample.clone());
        sample
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
            if state == "running" && job.attempts >= 3 {
                db.job_end(job.id, &Err("cut off by a restart 3 times".into()))?;
                continue;
            }
            db.job_again(job.id)?;
            let operator = self.clone();
            std::thread::spawn(move || {
                let db = match Db::open_default() {
                    Ok(db) => db,
                    Err(e) => return eprintln!("operator: job {} not resumed: {e:#}", job.id),
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
                    other => db.job_end(job.id, &Err(format!("no way to resume a {other} job"))),
                };
                if let Err(e) = outcome {
                    eprintln!("operator: job {} not recorded: {e:#}", job.id);
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
        let launch = store.resume_session(id, &resolved.config, None, &token, true)?;
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
        )
        .into_wire())
    }

    fn load(&self, _: &Ctx, _: Empty) -> Result<LoadState> {
        Ok(load::read_load(&SysRoot::from_env(), &config(None)?.config.load).into_wire())
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
        if let Ok(theirs) = self.system(async |p| p.version().await)
            && theirs != legionctl_machine::interface_version()
        {
            findings.push(
                "legionctl-system is built from another D-Bus interface than this operator; restart both after an \
                 upgrade (sudo systemctl restart legionctl-system; systemctl --user restart legionctl-operator)"
                    .into(),
            );
        }
        findings.extend(self.display_findings().iter().map(ToString::to_string));
        findings.extend(agent_findings()?);
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
        let outputs = gpu::outputs(&root);
        Ok(GpuState {
            mode: gpu::mode(&outputs).into_wire(),
            desktop_gpu: gpu::desktop_preference(&root).into_wire(),
            outputs: outputs.into_wire(),
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

    fn agents_usage(&self, _: &Ctx, input: AgentUsageIn) -> Result<UsageReport> {
        let (claude, codex) = match &input.account {
            Some(account) => {
                let profile = legionctl_core::session::account_dir(account)?;
                (profile.join("claude"), profile.join("codex"))
            }
            None => (home().join(".claude"), home().join(".codex")),
        };
        Ok(agents::usage(&claude, &codex, input.days).into_wire())
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

    fn agents_accounts_add(&self, _: &Ctx, input: AccountAddIn) -> Result<Lines> {
        anyhow::ensure!(
            legionctl_core::dossier::valid_name(&input.name),
            "an account name is a-z, 0-9 and '-', starting with a letter or digit"
        );
        let dir = legionctl_core::session::accounts_root().join(&input.name);
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
                    input.name
                ),
            ],
        })
    }

    fn workspace_open(&self, _: &Ctx, input: WorkspaceOpenIn) -> Result<Workspace> {
        let root = config(None)?.config.workspace.root;
        Ok(dossiers()?
            .open_workspace(
                session(&input.session)?,
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
            session(&input.session)?,
            input.repo.get()?,
            input.topic.as_deref(),
        )?;
        Ok(store.workspace_status(&ws)?.into_wire())
    }

    fn workspace_list(&self, _: &Ctx, _: Empty) -> Result<Workspaces> {
        Ok(dossiers()?.workspaces(None)?.into_wire())
    }

    fn workspace_checkpoint(&self, _: &Ctx, input: CheckpointIn) -> Result<Lines> {
        let store = dossiers()?;
        let ws = store.workspace_of(
            session(&input.session)?,
            input.repo.get()?,
            input.topic.as_deref(),
        )?;
        Ok(Lines {
            lines: vec![store.checkpoint(&ws, &input.message)?],
        })
    }

    fn workspace_merge(&self, _: &Ctx, input: WorkspaceIn) -> Result<Lines> {
        let store = dossiers()?;
        let ws = store.workspace_of(
            session(&input.session)?,
            input.repo.get()?,
            input.topic.as_deref(),
        )?;
        Ok(Lines {
            lines: store.merge(&ws)?,
        })
    }

    fn workspace_close(&self, _: &Ctx, input: CloseIn) -> Result<Lines> {
        let store = dossiers()?;
        let ws = store.workspace_of(
            session(&input.session)?,
            input.repo.get()?,
            input.topic.as_deref(),
        )?;
        Ok(Lines {
            lines: vec![store.close(&ws, input.force)?],
        })
    }

    fn setup_check(&self, _: &Ctx, _: Empty) -> Result<SetupReport> {
        Ok(setup::check().into_wire())
    }

    fn setup_apply(&self, _: &Ctx, input: SetupApplyIn) -> Result<Changes> {
        Ok(Changes {
            lines: setup::apply(input.install_ctx)?,
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
                if Event::from(input.event) == Event::SessionStart
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
        let ran = hooks::run(&resolved, input.event.into());
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
            Some(&place_of(input.repo.get()?)),
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
        Ok(dossiers()?.locks()?.into_wire())
    }

    fn dossier_list(&self, _: &Ctx, _: Empty) -> Result<DossierList> {
        Ok(dossiers()?.list().into_wire())
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
            input.mode.into(),
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
            None => ws
                .active(session(&input.session)?)?
                .context("this session has no active dossier; add --dossier <name>")?,
        };
        Ok(ws.graph(&dossier, input.node.as_deref())?.into_wire())
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
        Ok(skills::inventory(&home(), &config(Some(&input.project))?).into_wire())
    }

    fn monitor(&self, _: &Ctx, input: MonitorIn) -> Result<Samples> {
        anyhow::ensure!((1..=3600).contains(&input.count), "count must be 1-3600");
        anyhow::ensure!(
            (100..=60_000).contains(&input.interval_ms),
            "--interval-ms must be 100-60000"
        );
        let window = std::time::Duration::from_millis(u64::from(input.interval_ms));
        Ok((0..input.count)
            .map(|_| self.sample(window).into_wire())
            .collect())
    }

    fn sensors(&self, _: &Ctx, _: SensorsIn) -> Result<Readings> {
        let earlier: Vec<telemetry::Sample> = self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .collect();
        let now = self.sample(std::time::Duration::from_millis(250));
        let fan = self
            .system(async |p| p.fan_status().await)
            .ok()
            .and_then(|json| serde_json::from_str::<fan::FanState>(&json).ok());
        let (profile, _) = profile::load(None)?;
        Ok(legionctl_machine::sensors::view(
            &SysRoot::from_env(),
            &now,
            &earlier,
            fan.as_ref(),
            &profile.paths.ac_adapter,
        )
        .into_wire())
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
        Ok(CacheValue {
            value: Cache::open()?
                .get("tools", &key)?
                .map(|e| String::from_utf8_lossy(&e.value).into_owned()),
        })
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
                "created; start it with legionctl virt act start <name>"
            }
            .into(),
        );
        Ok(Lines { lines })
    }

    fn battery_status(&self, _: &Ctx, _: Empty) -> Result<legionctl_api::BatteryState> {
        let battery = legionctl_machine::battery::state(&SysRoot::from_env());
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
            crate::display::findings(&SysRoot::from_env(), &home(), &monitors, &antialiasing)
                .iter()
                .map(|f| format!("- {f}")),
        );
        Ok(Lines { lines })
    }

    fn display_fix(&self, ctx: &Ctx, input: DisplayFixIn) -> Result<Changes> {
        let all = input.finding == "all";
        let found: Vec<Finding> = self
            .display_findings()
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
        self.boot_action("install", input.dry_run, |root| {
            let mut plan = legionctl_machine::boot::install_plan(root)?;
            plan.push(legionctl_machine::policy::Step::Info(
                "then the next start tries legionctl's entry once (BootNext)".into(),
            ));
            Ok(plan)
        })
    }

    fn boot_confirm(&self, _: &Ctx, input: DryRunIn) -> Result<Changes> {
        self.boot_action("confirm", input.dry_run, |root| {
            legionctl_machine::boot::confirm_plan(&legionctl_machine::boot::state(root)?)
        })
    }

    fn boot_rebuild(&self, _: &Ctx, input: DryRunIn) -> Result<Changes> {
        self.boot_action("rebuild", input.dry_run, |root| {
            legionctl_machine::boot::rebuild_plan(root, &legionctl_machine::boot::state(root)?)
        })
    }

    fn boot_remove(&self, _: &Ctx, input: DryRunIn) -> Result<Changes> {
        self.boot_action("remove", input.dry_run, |root| {
            legionctl_machine::boot::remove_plan(root, &legionctl_machine::boot::state(root)?)
        })
    }

    fn kernel_list(&self, _: &Ctx, _: Empty) -> Result<legionctl_api::Kernels> {
        Ok(legionctl_machine::kernel::list(&SysRoot::from_env()).into_wire())
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
        Ok(legionctl_machine::greeter::state(&SysRoot::from_env()).into_wire())
    }

    fn greeter_install(&self, _: &Ctx, input: DryRunIn) -> Result<Changes> {
        self.greeter_action(
            "install",
            input.dry_run,
            legionctl_machine::greeter::install_plan,
        )
    }

    fn greeter_remove(&self, _: &Ctx, input: DryRunIn) -> Result<Changes> {
        self.greeter_action(
            "remove",
            input.dry_run,
            legionctl_machine::greeter::remove_plan,
        )
    }

    fn windows_status(&self, _: &Ctx, _: Empty) -> Result<WindowsState> {
        Ok(windows::state(&SysRoot::from_env())?.into_wire())
    }

    fn windows_reboot(&self, _: &Ctx, input: DryRunIn) -> Result<Changes> {
        if input.dry_run {
            return Ok(dry_run(&windows::reboot_plan(&windows::state(
                &SysRoot::from_env(),
            )?)?));
        }
        Ok(Changes {
            lines: self.system(async |p| p.reboot_windows().await)?,
            dry_run: false,
        })
    }

    fn windows_vm_start(&self, _: &Ctx, _: Empty) -> Result<Lines> {
        let unit = format!("{}.service", legionctl_core::session::WINDOWS_UNIT);
        anyhow::ensure!(
            !unit_active(&unit),
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
            let now = std::fs::read_to_string(&state).unwrap_or_default();
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
        if unit_active(&unit) {
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
            unit_active(&unit),
            "the Windows VM does not run (legionctl windows vm start)"
        );
        let dir = legionctl_core::session::windows_root();
        let spice = dir.join("spice.sock");
        // with the GPU, Windows draws on it: Looking Glass shows its frames; else the SPICE display
        let (viewer, args, shown) = if windows_has_gpu(&dir.join("qmp.sock")) {
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
        let state =
            std::fs::read_to_string(dir.join("state")).unwrap_or_else(|_| "never started".into());
        let mut lines = vec![if unit_active(&unit) {
            "running".to_string()
        } else {
            format!(
                "not running (last: {})",
                state.lines().next().unwrap_or_default()
            )
        }];
        if unit_active(&unit) {
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
        let (lines, devices, msdm) =
            self.system(async move |p| p.windows_vm_begin(&usb, input.pid, memlock).await)?;
        Ok(WindowsVmBegun {
            lines,
            devices,
            msdm,
        })
    }

    fn internal_windows_vm_end(&self, _: &Ctx, _: Empty) -> Result<Lines> {
        Ok(Lines {
            lines: self.system(async |p| p.windows_vm_end().await)?,
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
            .collect::<Vec<_>>()
            .into_wire();
        // status reads more files than the decisions do; recording them lets every read operation replay
        legionctl_machine::snapshot::Snapshot::read(
            &plan.root,
            &plan.profile,
            Detail::Full,
            &config(None)?.config.load,
        );
        let mut recording = plan.decisions(dir, events)?;
        let tree = dir.join("root");
        recording.paths = root.record_into(&tree)?;
        let layers = plan.layers.iter().map(|l| l.file.as_str());
        for file in [profile::DEFAULT_PATH, legionctl_machine::ledger::DB]
            .into_iter()
            .chain(layers)
        {
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
        // the log keeps the domain value; the answer is its wire form (bench_run_of)
        let result = bench::BenchResult {
            scenario: input.scenario.into(),
            label: input.label,
            seconds: input.seconds,
            scores: scores.scores,
            problems: scores.problems,
            summary: bench::summarize(&samples),
            knobs,
            kernel: legionctl_machine::kernel::running_package(&SysRoot::from_env()),
            release: Some(legionctl_machine::kernel::running_kernel(
                &SysRoot::from_env(),
            )),
        };
        let db = Db::open_default()?;
        let id = db.record(&ctx.actor, "bench.result", &serde_json::to_value(&result)?)?;
        bench_run_of(db.event(id)?.context("the run was not stored")?)
    }

    fn tune(&self, _: &Ctx, input: TuneIn) -> Result<TuneReport> {
        let load = load::read_load(&SysRoot::from_env(), &config(None)?.config.load);
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
        Ok(knobs::catalog(&Plan::load(path(input.profile.as_ref())?)?.machine()).into_wire())
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
        let knobs = knobs::catalog(&m)
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
            conditions: conditions(&m),
            knobs,
            changes: dry_run(&m.apply(None, keep)?.1).lines,
        })
    }

    fn rules_list(&self, _: &Ctx, _: Empty) -> Result<Rules> {
        let plan = Plan::load(None)?;
        let m = plan.machine();
        Ok(Rules {
            conditions: conditions(&m),
            layers: plan.layers.iter().map(|l| rule_state(&m, l)).collect(),
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
            layer: rule_state(&m, layer),
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
            Some(k) => Some(knobs::find(&m, k).ok_or_else(|| {
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
                for layer in plan
                    .layers
                    .iter()
                    .filter(|l| m.ledger.layer_enabled(&l.id()).is_some())
                {
                    let id = layer.id();
                    lines.extend(self.system(async |p| p.set_layer_state(&id, "default").await)?);
                }
            }
        } else {
            for layer in rules::own_layers(&plan.root, &plan.profile, &home())
                .iter()
                .filter_map(reset)
            {
                lines.push(rules::write_own(&layer)?);
            }
            let held = m.ledger.holds().into_keys().filter(|path| {
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
        let ledger = Ledger::open(&SysRoot::from_env())?;
        let to = ledger.sets().remove(&input.name).ok_or_else(|| {
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
                lines: legionctl_machine::sets::changes(&ledger.selection(), &to),
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

    fn knobs_show(&self, _: &Ctx, input: InspectIn) -> Result<KnobDetail> {
        let plan = Plan::load(path(input.profile.as_ref())?)?;
        let knob = knobs::find(&plan.machine(), &input.knob).with_context(|| {
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
            inside: input.inside.into(),
            gpu: input.gpu,
        };
        let launch = store.start_session(&start, &resolved.config)?;
        Db::open_default()?.issue_token(&launch.id, &token)?;
        Ok(launch.into_wire())
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
            inside: input.inside.into(),
            gpu: input.gpu,
        };
        let launch = store.start_session(&start, &resolved.config)?;
        let db = Db::open_default()?;
        db.issue_token(&launch.id, &token)?;
        let job_input =
            serde_json::json!({"session": launch.id, "prompt": input.prompt, "tag": input.tag});
        let job = db.job_begin("session.run", &job_input)?;
        let out = self.runtime.block_on(crate::acp::run(
            launch,
            &input.prompt,
            (job, &job_input),
            stoppable(job),
            &store,
        ));
        STOPS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&job);
        end_job(&db, job, &out)?;
        out
    }

    fn session_list(&self, _: &Ctx, _: Empty) -> Result<Sessions> {
        Ok(dossiers()?
            .sessions()?
            .into_iter()
            .map(|session| {
                let state = legionctl_core::session::state(&session.id, session.inside);
                SessionState {
                    state,
                    session: session.into_wire(),
                }
            })
            .collect())
    }

    fn session_stop(&self, _: &Ctx, input: SessionIdIn) -> Result<Lines> {
        session_control(legionctl_core::session::Control::Stop, &input.id, "stopped")
    }

    fn session_activity(&self, _: &Ctx, input: SessionActivityIn) -> Result<Page<SessionExec>> {
        let (after, limit) = page_of(input.cursor.as_deref(), input.limit)?;
        let filter = EventFilter {
            kinds: &["session.exec"],
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
                        argv: serde_json::from_value(e.payload["argv"].clone()).ok()?,
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
            false,
        )?;
        Db::open_default()?.issue_token(&launch.id, &token)?;
        Ok(launch.into_wire())
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
        Ok(config(Some(&input.project))?
            .explain(&input.key)
            .into_wire())
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
