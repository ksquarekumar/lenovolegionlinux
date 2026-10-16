//! VFIO passthrough (decided 2026-10-14: GPU per run in a session VM, and the installed Windows in a VM): a whole IOMMU
//! group goes to `vfio-pci` for one VM of one user, and back to its own driver after. Refused while the host uses a
//! device of the group: a driver taken from under a process that holds its device node can hang the machine (NVIDIA)
//! or lose data (a mounted disk).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::sysroot::{SysRoot, denied};

const PCI: &str = "/sys/bus/pci/devices";
const VFIO: &str = "vfio-pci";

/// A PCI address as the kernel writes it (`0000:01:00.0`).
fn valid(device: &str) -> bool {
    let b = device.as_bytes();
    b.len() == 12
        && [4, 7].iter().all(|&i| b[i] == b':')
        && b[10] == b'.'
        && device
            .chars()
            .enumerate()
            .all(|(i, c)| [4, 7, 10].contains(&i) || c.is_ascii_hexdigit())
}

/// A driver's folder, named by itself (not through a device's `driver` link, which a recording can point anywhere).
fn drivers(name: &str) -> PathBuf {
    Path::new("/sys/bus/pci/drivers").join(name)
}

fn device_path(device: &str) -> PathBuf {
    Path::new(PCI).join(device)
}

fn name(path: &Path) -> Option<String> {
    path.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// The driver bound to `device` now, if any.
///
/// # Errors
/// The `driver` link cannot be read.
pub fn driver(root: &SysRoot, device: &str) -> Result<Option<String>> {
    Ok(root
        .link(device_path(device).join("driver"))?
        .as_deref()
        .and_then(name))
}

/// The IOMMU group of `device` and every device in it, sorted.
///
/// # Errors
/// `device` is not a PCI address of this machine, or it has no IOMMU group (the IOMMU is off).
pub fn group(root: &SysRoot, device: &str) -> Result<(String, Vec<String>)> {
    if !valid(device) || !root.exists(device_path(device))? {
        bail!("{device} is not a PCI device of this machine (see lspci -D)");
    }
    let group = iommu_group(root, device)?.with_context(|| {
        format!(
            "{device} has no IOMMU group: turn on the IOMMU (intel_iommu=on, VT-d in the firmware)"
        )
    })?;
    Ok((group.clone(), members(root, &group)?))
}

fn iommu_group(root: &SysRoot, device: &str) -> Result<Option<String>> {
    Ok(root
        .link(device_path(device).join("iommu_group"))?
        .as_deref()
        .and_then(name))
}

fn members(root: &SysRoot, group: &str) -> Result<Vec<String>> {
    Ok(root
        .children(format!("/sys/kernel/iommu_groups/{group}/devices"), "")?
        .iter()
        .filter_map(|p| name(p))
        .collect())
}

/// The device nodes of `device` that a host process can hold (DRM nodes, NVIDIA's nodes for an NVIDIA GPU, an `NVMe`
/// controller), and its block devices (namespaces, partitions). None for an audio function: sound cards are made to
/// go away while in use.
fn nodes(root: &SysRoot, device: &str) -> Result<(Vec<String>, Vec<String>)> {
    let dir = device_path(device);
    let class = root
        .read(dir.join("class"))?
        .with_context(|| format!("{device} has no class: the host-use check cannot run"))?;
    let names = |sub: &str, prefix: &str| -> Result<Vec<String>> {
        Ok(root
            .children(dir.join(sub), prefix)?
            .iter()
            .filter_map(|p| name(p))
            .collect())
    };
    let (mut nodes, mut blocks): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    if class.starts_with("0x03") {
        nodes.extend(
            names("drm", "")?
                .into_iter()
                .map(|n| format!("/dev/dri/{n}")),
        );
        if root.read(dir.join("vendor"))?.as_deref() == Some("0x10de") {
            nodes.extend(
                root.children("/dev", "nvidia")?
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned()),
            );
        }
    } else if class.starts_with("0x0108") {
        for controller in names("nvme", "nvme")? {
            nodes.push(format!("/dev/{controller}"));
            for namespace in names(&format!("nvme/{controller}"), &format!("{controller}n"))? {
                blocks.push(format!("/dev/{namespace}"));
                blocks.extend(
                    root.children(format!("/sys/block/{namespace}"), &format!("{namespace}p"))?
                        .iter()
                        .filter_map(|p| p.file_name())
                        .map(|n| format!("/dev/{}", n.to_string_lossy())),
                );
            }
        }
    }
    nodes.extend(blocks.iter().cloned());
    Ok((nodes, blocks))
}

/// Why the group of `devices` cannot go to a VM now, one line each: processes that hold a device node, mounted, swap
/// or held block devices. Empty when nothing on the host uses it. With `all`, each process must be read (the guard,
/// as root); without, the processes that the reader may not see are left out (a report as a user).
///
/// # Errors
/// A file that the check needs cannot be read: the host-use check cannot run.
pub fn users(root: &SysRoot, devices: &[String], all: bool) -> Result<Vec<String>> {
    let (mut held_nodes, mut blocks) = (Vec::new(), Vec::new());
    for device in devices {
        let (dn, db) = nodes(root, device)?;
        held_nodes.extend(dn);
        blocks.extend(db);
    }
    let mut out = Vec::new();
    for proc in root.children("/proc", "")? {
        let Some(pid) = name(&proc) else {
            continue;
        };
        if !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let fds = match root.children(proc.join("fd"), "") {
            Err(e) if !all && denied(&e) => continue,
            fds => fds?,
        };
        let mut held = None;
        for fd in fds {
            if let Some(link) = root.link(&fd)?.map(|l| l.to_string_lossy().into_owned())
                && held_nodes.contains(&link)
            {
                held = Some(link);
                break;
            }
        }
        if let Some(node) = held {
            // a process that ended since the listing has no comm
            let comm = root.read(proc.join("comm"))?.unwrap_or_default();
            out.push(format!("{comm} (pid {pid}) uses {node}"));
        }
    }
    for (file, what) in [
        ("/proc/self/mounts", "is mounted at"),
        ("/proc/swaps", "is swap"),
    ] {
        let table = root
            .read(file)
            .and_then(|t| t.with_context(|| format!("{file} is missing")))
            .with_context(|| format!("cannot read {file}: the host-use check cannot run"))?;
        for line in table.lines() {
            let mut fields = line.split_whitespace();
            let (Some(source), at) = (fields.next(), fields.next().unwrap_or_default()) else {
                continue;
            };
            if blocks.iter().any(|b| b == source) {
                out.push(format!("{source} {what} {at}").trim_end().to_string());
            }
        }
    }
    for block in &blocks {
        let name = block.trim_start_matches("/dev/");
        let namespace = name.split_once('p').map_or(name, |(n, _)| n);
        let holders = if name == namespace {
            format!("/sys/block/{name}/holders")
        } else {
            format!("/sys/block/{namespace}/{name}/holders")
        };
        for holder in root.children(&holders, "")? {
            let by = holder
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            out.push(format!("{block} is held by {by} (device mapper or RAID)"));
        }
    }
    Ok(out)
}

fn write(root: &SysRoot, path: &Path, value: &str) -> Result<()> {
    std::fs::write(root.path(path), value)
        .with_context(|| format!("writing {value} to {}", path.display()))
}

/// Wait until `device`'s driver is (or, with `away`, is no longer) `vfio-pci`.
fn settle(root: &SysRoot, device: &str, away: bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while (driver(root, device)?.as_deref() == Some(VFIO)) == away {
        if Instant::now() > deadline {
            bail!(
                "the kernel did not {} {device} in 5 s (driver now: {})",
                if away {
                    "give back"
                } else {
                    "bind vfio-pci to"
                },
                driver(root, device)?.unwrap_or_else(|| "none".into())
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

/// Check that `devices` are one whole IOMMU group that the host does not use; returns the group.
///
/// # Errors
/// A device is unknown, the devices are not exactly one group, or the host uses the group (the reasons).
pub fn check(root: &SysRoot, devices: &[String]) -> Result<String> {
    let first = devices.first().context("no devices")?;
    let (group, members) = group(root, first)?;
    let mut asked = devices.to_vec();
    asked.sort();
    asked.dedup();
    if asked != members {
        bail!(
            "IOMMU group {group} is {}, not {}: a VM gets a whole group, so give exactly its devices",
            members.join(" "),
            asked.join(" ")
        );
    }
    let users = users(root, devices, true)?;
    if !users.is_empty() {
        bail!(
            "the host uses IOMMU group {group}:\n- {}",
            users.join("\n- ")
        );
    }
    Ok(group)
}

/// Bind `devices` (one whole group, checked) to `vfio-pci` and give `/dev/vfio/<group>` to `uid`; returns what it did.
///
/// # Errors
/// `check` fails, a sysfs write fails, or the kernel does not bind in time.
pub fn bind(root: &SysRoot, devices: &[String], uid: u32) -> Result<Vec<String>> {
    let group = check(root, devices)?;
    if !root.exists("/sys/bus/pci/drivers/vfio-pci")? {
        let loaded = std::process::Command::new("modprobe").arg(VFIO).status()?;
        anyhow::ensure!(loaded.success(), "modprobe {VFIO} failed ({loaded})");
    }
    let mut lines = Vec::new();
    for device in devices {
        let dir = device_path(device);
        write(root, &dir.join("driver_override"), VFIO)?;
        if let Some(old) = driver(root, device)?.filter(|d| d != VFIO) {
            write(root, &drivers(&old).join("unbind"), device)?;
            lines.push(format!("{device}: {old} -> {VFIO}"));
        }
        write(root, Path::new("/sys/bus/pci/drivers_probe"), device)?;
        settle(root, device, false)?;
    }
    let node = root.path(format!("/dev/vfio/{group}"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !node.exists() {
        if Instant::now() > deadline {
            bail!("/dev/vfio/{group} did not appear");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    rustix::fs::chown(&node, Some(rustix::fs::Uid::from_raw(uid)), None)
        .with_context(|| format!("giving /dev/vfio/{group} to uid {uid}"))?;
    lines.push(format!("/dev/vfio/{group}: uid {uid}"));
    Ok(lines)
}

/// Give `devices` back to their own drivers (the kernel probes them again); returns what it did.
///
/// # Errors
/// A device is not on `vfio-pci`, a sysfs write fails, or the kernel does not give it back in time.
pub fn unbind(root: &SysRoot, devices: &[String]) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for device in devices {
        if driver(root, device)?.as_deref() != Some(VFIO) {
            bail!("{device} is not on {VFIO}");
        }
        let dir = device_path(device);
        write(root, &dir.join("driver_override"), "\n")?;
        write(root, &drivers(VFIO).join("unbind"), device)?;
        write(root, Path::new("/sys/bus/pci/drivers_probe"), device)?;
        settle(root, device, true)?;
        lines.push(format!(
            "{device}: {VFIO} -> {}",
            driver(root, device)?.unwrap_or_else(|| "none".into())
        ));
    }
    Ok(lines)
}

/// Let process `pid` of user `uid` (the VM runner, before it starts QEMU) lock `bytes` of memory: QEMU locks all of a
/// VM's memory for VFIO, far above a user's limit.
///
/// # Errors
/// `pid` is not a process of `uid`, or the limit cannot be set.
pub fn allow_memlock(root: &SysRoot, pid: u32, uid: u32, bytes: u64) -> Result<String> {
    let status = root
        .read(format!("/proc/{pid}/status"))?
        .with_context(|| format!("no process {pid}"))?;
    let owner = status
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))
        .and_then(|ids| ids.split_whitespace().next())
        .and_then(|id| id.parse::<u32>().ok());
    if owner != Some(uid) {
        bail!("process {pid} is not a process of uid {uid}");
    }
    let target = rustix::process::Pid::from_raw(i32::try_from(pid)?).context("pid 0")?;
    let limit = rustix::process::Rlimit {
        current: Some(bytes),
        maximum: Some(bytes),
    };
    rustix::process::prlimit(Some(target), rustix::process::Resource::Memlock, limit)
        .with_context(|| format!("setting the memory lock limit of process {pid}"))?;
    Ok(format!("process {pid}: may lock {} MiB", bytes >> 20))
}

/// A device that can go to a VM: a discrete GPU or an `NVMe` controller.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Candidate {
    pub device: String,
    /// gpu or nvme.
    pub kind: String,
    /// PCI vendor and device ids (`10de:27e0`).
    pub ids: String,
    pub group: String,
    pub members: Vec<String>,
    pub driver: Option<String>,
    /// What on the host uses its group now (complete only as root: other users' open files).
    pub users: Vec<String>,
}

/// The discrete GPUs (not on i915 or xe) and `NVMe` controllers of this machine that have an IOMMU group, with their
/// groups and users.
///
/// # Errors
/// A device's files cannot be read.
pub fn candidates(root: &SysRoot) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    for dir in root.children(PCI, "")? {
        let Some(device) = name(&dir) else { continue };
        let Some(class) = root.read(dir.join("class"))? else {
            continue;
        };
        let driver = driver(root, &device)?;
        let kind = if class.starts_with("0x03") && !matches!(driver.as_deref(), Some("i915" | "xe"))
        {
            "gpu"
        } else if class.starts_with("0x0108") {
            "nvme"
        } else {
            continue;
        };
        let Some(group) = iommu_group(root, &device)? else {
            continue;
        };
        let members = members(root, &group)?;
        let id = |f: &str| -> Result<String> {
            Ok(root
                .read(dir.join(f))?
                .unwrap_or_default()
                .trim_start_matches("0x")
                .to_string())
        };
        out.push(Candidate {
            ids: format!("{}:{}", id("vendor")?, id("device")?),
            device,
            kind: kind.into(),
            group,
            users: users(root, &members, false)?,
            members,
            driver,
        });
    }
    Ok(out)
}

/// NVIDIA's services that hold the GPU's device nodes; stopped while the GPU is not Linux's.
const NVIDIA_SERVICES: [&str; 2] = ["nvidia-persistenced.service", "nvidia-powerd.service"];

fn systemctl(verb: &str, units: &[String]) -> Result<()> {
    if units.is_empty() {
        return Ok(());
    }
    let out = std::process::Command::new("systemctl")
        .arg(verb)
        .args(units)
        .output()?;
    anyhow::ensure!(
        out.status.success(),
        "systemctl {verb} {}: {}",
        units.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

/// The NVIDIA GPU's whole IOMMU group.
///
/// # Errors
/// No NVIDIA GPU with an IOMMU group.
pub fn nvidia_group(root: &SysRoot) -> Result<Vec<String>> {
    nvidia(root)?.context("no NVIDIA GPU with an IOMMU group on this machine")
}

/// The NVIDIA GPU's whole IOMMU group; None without an NVIDIA GPU with an IOMMU group.
///
/// # Errors
/// A device's files cannot be read.
pub fn nvidia(root: &SysRoot) -> Result<Option<Vec<String>>> {
    Ok(candidates(root)?
        .into_iter()
        .find(|c| c.kind == "gpu" && c.ids.starts_with("10de:"))
        .map(|c| c.members))
}

/// Take the NVIDIA GPU from Linux: its services stopped, its group bound to vfio-pci for `uid`. Returns what it did
/// and the services to start again at the return; on a refusal the services start again.
///
/// # Errors
/// The host still uses the GPU (the reasons), or a step fails.
pub fn release_nvidia(root: &SysRoot, uid: u32) -> Result<(Vec<String>, Vec<String>)> {
    let group = nvidia_group(root)?;
    let active: Vec<String> = NVIDIA_SERVICES
        .iter()
        .filter(|s| {
            std::process::Command::new("systemctl")
                .args(["is-active", "-q", s])
                .status()
                .is_ok_and(|st| st.success())
        })
        .map(|s| (*s).to_string())
        .collect();
    systemctl("stop", &active)?;
    match bind(root, &group, uid) {
        Ok(mut lines) => {
            lines.extend(active.iter().map(|s| format!("{s}: stopped")));
            Ok((lines, active))
        }
        Err(e) => {
            let again = systemctl("start", &active);
            let note = again.err().map_or_else(String::new, |e| {
                format!(" (and starting {} again: {e:#})", active.join(" "))
            });
            Err(e.context(format!(
                "the NVIDIA GPU stays with Linux{note}; if Mutter holds it, run `legionctl gpu primary intel --release-nvidia` \
                 and log in again"
            )))
        }
    }
}

/// Give the NVIDIA GPU back to Linux: its group back to `nvidia`, the services that `release_nvidia` stopped started.
///
/// # Errors
/// The group is not on vfio-pci, or a step fails.
pub fn return_nvidia(root: &SysRoot, services: &[String]) -> Result<Vec<String>> {
    let mut lines = unbind(root, &nvidia_group(root)?)?;
    systemctl("start", services)?;
    lines.extend(services.iter().map(|s| format!("{s}: started")));
    Ok(lines)
}
