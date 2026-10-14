//! The installed Windows in a VM (note 05, decided by the user 2026-10-14): the daemon makes the host ready for it and
//! undoes that after. Windows gets its own `NVMe` controller (VFIO); the host must not use it while it runs: no mounted
//! or automounted partition of that disk, no hibernated Windows, no one-time boot into Windows. The GPU moves to and
//! from the running VM on its own (`vfio`).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::policy::Step;
use crate::sysroot::SysRoot;
use crate::{vfio, windows};

/// The user service that runs the Windows VM.
pub const UNIT: &str = "legionctl-windows-vm";

/// What a Windows VM asks of the host.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ask {
    /// USB devices by vendor:product (`046d:c548`).
    pub usb: Vec<String>,
    /// The VM runner, and the memory it may lock.
    pub pid: u32,
    pub memlock: u64,
}

/// What a start changed on the host, kept in the ledger so the end (or a daemon that started again) undoes it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Record {
    /// Automount units stopped (systemd names).
    pub automounts: Vec<String>,
    /// PCI devices bound to vfio-pci.
    pub devices: Vec<String>,
    /// USB device nodes given to the user.
    pub usb_nodes: Vec<String>,
}

/// The block device name (`nvme0n1p3`) of a mount source (`/dev/...`, `UUID=...`, `PARTUUID=...`).
fn block(root: &SysRoot, source: &str) -> Option<String> {
    let link = if let Some((kind, id)) = source.split_once('=') {
        let dir = match kind {
            "UUID" => "uuid",
            "PARTUUID" => "partuuid",
            "LABEL" => "label",
            _ => return None,
        };
        root.link(format!("/dev/disk/by-{dir}/{id}"))?
    } else {
        PathBuf::from(source)
    };
    Some(link.file_name()?.to_string_lossy().into_owned())
}

/// The PCI address of the `NVMe` controller of block device `name` (`nvme0n1p3` -> `nvme0`).
fn controller(root: &SysRoot, name: &str) -> Option<String> {
    let rest = name.strip_prefix("nvme")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    root.link(format!("/sys/class/nvme/nvme{digits}/device"))
        .and_then(|l| l.file_name().map(|n| n.to_string_lossy().into_owned()))
}

/// The `NVMe` controller that holds the Windows volume.
///
/// # Errors
/// No NTFS volume with the SYSTEM hive in the mount table or fstab, or it is not on an `NVMe` controller.
pub fn windows_disk(root: &SysRoot) -> Result<String> {
    let (source, target) = windows::volume(root).context(
        "no Windows volume: no NTFS entry of the mount table or /etc/fstab holds Windows\\System32\\config\\SYSTEM",
    )?;
    block(root, &source)
        .and_then(|b| controller(root, &b))
        .with_context(|| {
            format!(
                "the Windows volume {source} ({}) is not on an NVMe controller",
                target.display()
            )
        })
}

/// The systemd unit name of a mount point (`/mnt/windows-system` -> `mnt-windows\x2dsystem`), as `systemd-escape --path`.
fn unit_name(target: &Path) -> String {
    target
        .to_string_lossy()
        .trim_matches('/')
        .split('/')
        .map(|part| {
            part.bytes()
                .enumerate()
                .map(|(i, b)| match b {
                    b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b':' | b'_' => {
                        char::from(b).to_string()
                    }
                    b'.' if i > 0 => ".".into(),
                    _ => format!("\\x{b:02x}"),
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("-")
}

/// The fstab mount points of NTFS (or any) partitions on the `NVMe` controller `disk`.
fn fstab_targets(root: &SysRoot, disk: &str) -> Vec<PathBuf> {
    root.read("/etc/fstab")
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| {
            let mut fields = l.split_whitespace();
            let source = fields.next()?;
            let target = fields.next()?;
            (block(root, source)
                .and_then(|b| controller(root, &b))
                .as_deref()
                == Some(disk))
            .then(|| PathBuf::from(target.replace("\\040", " ")))
        })
        .collect()
}

/// The mount points of the Windows disk's partitions in `/etc/fstab`: what `disk_plan` attaches and detaches.
///
/// # Errors
/// No Windows disk.
pub fn disk_targets(root: &SysRoot) -> Result<Vec<PathBuf>> {
    Ok(fstab_targets(root, &windows_disk(root)?))
}

/// The steps that give the Windows disk's partitions to Linux (`attach`: their automounts started) or take them away
/// (their automounts and mounts stopped), as a VM start does.
///
/// # Errors
/// No Windows disk, or no fstab entry on it.
pub fn disk_plan(root: &SysRoot, attach: bool) -> Result<Vec<Step>> {
    let disk = windows_disk(root)?;
    let units: Vec<String> = fstab_targets(root, &disk)
        .iter()
        .flat_map(|target| {
            let unit = unit_name(target);
            if attach {
                vec![format!("{unit}.automount")]
            } else {
                vec![format!("{unit}.automount"), format!("{unit}.mount")]
            }
        })
        .collect();
    anyhow::ensure!(
        !units.is_empty(),
        "no /etc/fstab entry is on the Windows disk {disk}"
    );
    let verb = if attach { "start" } else { "stop" };
    Ok(vec![Step::Run(
        std::iter::once("systemctl".to_string())
            .chain(std::iter::once(verb.to_string()))
            .chain(units)
            .collect(),
    )])
}

/// Whether Windows is hibernated on its volume (mounted at `target`): `hiberfil.sys` starts with `hibr` (a resume
/// image); after a full shut down or a resume it starts with zeros or `wake`.
fn hibernated(root: &SysRoot, target: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 4];
    std::fs::File::open(root.path(target).join("hiberfil.sys"))
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok_and(|()| head.eq_ignore_ascii_case(b"hibr"))
}

fn systemctl(verb: &str, units: &[String]) -> Result<()> {
    if units.is_empty() {
        return Ok(());
    }
    let out = Command::new("systemctl").arg(verb).args(units).output()?;
    anyhow::ensure!(
        out.status.success(),
        "systemctl {verb} {}: {}",
        units.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

/// The device nodes (`/dev/bus/usb/BBB/DDD`) of the USB devices `ids` (vendor:product).
fn usb_nodes(root: &SysRoot, ids: &[String]) -> Result<Vec<String>> {
    ids.iter()
        .map(|id| {
            let (vendor, product) = id
                .split_once(':')
                .with_context(|| format!("{id} is not vendor:product"))?;
            root.children("/sys/bus/usb/devices", "")
                .iter()
                .find(|d| {
                    root.read(d.join("idVendor")).as_deref() == Some(vendor)
                        && root.read(d.join("idProduct")).as_deref() == Some(product)
                })
                .and_then(|d| {
                    let bus: u32 = root.read(d.join("busnum"))?.parse().ok()?;
                    let dev: u32 = root.read(d.join("devnum"))?.parse().ok()?;
                    Some(format!("/dev/bus/usb/{bus:03}/{dev:03}"))
                })
                .with_context(|| format!("no USB device {id} is plugged in (lsusb)"))
        })
        .collect()
}

fn chown(root: &SysRoot, node: &str, uid: u32) -> Result<()> {
    rustix::fs::chown(root.path(node), Some(rustix::fs::Uid::from_raw(uid)), None)
        .with_context(|| format!("giving {node} to uid {uid}"))
}

/// Why Windows cannot start in a VM now, before anything changes: a one-time boot into it, Fast Startup, a
/// hibernated Windows.
fn refusals(root: &SysRoot) -> Result<Vec<String>> {
    let state = windows::state(root)?;
    let mut why = Vec::new();
    if state.boot_next.is_some() && state.boot_next == state.boot_entry {
        why.push("the next start of the machine goes into Windows (BootNext): cancel it with efibootmgr --delete-bootnext".into());
    }
    if state.fast_startup == Some(true) {
        why.push("Fast Startup is on: Windows hibernates at shut down; turn it off in Windows (powercfg /h off)".into());
    }
    if let Some((_, target)) = windows::volume(root)
        && hibernated(root, &target)
    {
        why.push(format!("Windows is hibernated ({}/hiberfil.sys): start it on the machine and shut it down fully", target.display()));
    }
    Ok(why)
}

/// Make the host ready for the Windows VM of `uid` and return the lines and the record. On a refusal or a failure
/// half way, what was done is undone.
///
/// # Errors
/// A refusal (with its reasons) or a failed step.
pub fn begin(root: &SysRoot, uid: u32, ask: &Ask) -> Result<(Vec<String>, Record)> {
    let why = refusals(root)?;
    if !why.is_empty() {
        bail!("the Windows VM cannot start:\n- {}", why.join("\n- "));
    }
    let disk = windows_disk(root)?;
    let mut record = Record::default();
    let mut lines = Vec::new();
    let done = (|| -> Result<()> {
        for target in fstab_targets(root, &disk) {
            let unit = unit_name(&target);
            record.automounts.push(format!("{unit}.automount"));
            systemctl(
                "stop",
                &[format!("{unit}.automount"), format!("{unit}.mount")],
            )?;
            lines.push(format!(
                "{}: unmounted, automount stopped",
                target.display()
            ));
        }
        let (_, group) = vfio::group(root, &disk)?;
        lines.extend(vfio::bind(root, &group, uid)?);
        record.devices.extend(group);
        for node in usb_nodes(root, &ask.usb)? {
            chown(root, &node, uid)?;
            lines.push(format!("{node}: uid {uid}"));
            record.usb_nodes.push(node);
        }
        lines.push(vfio::allow_memlock(root, ask.pid, uid, ask.memlock)?);
        Ok(())
    })();
    if let Err(e) = done {
        let undone = end(root, &record);
        bail!("{e:#}\nundone:\n- {}", undone.join("\n- "));
    }
    Ok((lines, record))
}

/// Undo what `begin` recorded: devices back to their drivers, USB nodes back to root, automounts started again; each line says what happened (a failed step does not stop the others).
#[must_use]
pub fn end(root: &SysRoot, record: &Record) -> Vec<String> {
    let mut lines = Vec::new();
    let mut note = |what: String, result: Result<()>| {
        lines.push(match result {
            Ok(()) => what,
            Err(e) => format!("warning: {what}: {e:#}"),
        });
    };
    for device in &record.devices {
        let back = vfio::unbind(root, std::slice::from_ref(device)).map(|_| ());
        note(format!("{device}: given back"), back);
    }
    for node in &record.usb_nodes {
        note(format!("{node}: back to root"), chown(root, node, 0));
    }
    if !record.automounts.is_empty() {
        let started = systemctl("start", &record.automounts);
        note(format!("started {}", record.automounts.join(" ")), started);
    }
    lines
}

/// The `MSDM` table (the Windows license of this machine), for the VM's ACPI tables; root-only on the host.
///
/// # Errors
/// The firmware has no `MSDM` table.
pub fn msdm(root: &SysRoot) -> Result<Vec<u8>> {
    std::fs::read(root.path("/sys/firmware/acpi/tables/MSDM"))
        .context("the firmware has no MSDM table")
}

/// The polkit rule that lets user `user` start and end the Windows VM without a prompt (autostart: at boot no
/// polkit agent can ask, and a prompt at every login is no autostart). Only this action, only this user.
pub const AUTOSTART_RULE: &str = "/etc/polkit-1/rules.d/49-legionctl-windows-vm.rules";

/// The autostart rule of `user`.
///
/// # Errors
/// The user name has characters a rule cannot hold.
pub fn autostart_rule(user: &str) -> Result<String> {
    anyhow::ensure!(
        !user.is_empty()
            && user
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "{user} is not a user name a rule can hold"
    );
    Ok(format!(
        "// legionctl windows vm autostart: {user} may start and end the Windows VM without a prompt\n\
         polkit.addRule(function(action, subject) {{\n    \
         if (action.id == \"org.legionctl.windows.vm\" && subject.user == \"{user}\") {{\n        \
         return polkit.Result.YES;\n    }}\n}});\n"
    ))
}

/// Write (`Some(user)`) or remove (`None`) the autostart rule.
///
/// # Errors
/// The user name has characters a rule cannot hold, or the file cannot be written or removed.
pub fn set_autostart_rule(root: &SysRoot, user: Option<&str>) -> Result<String> {
    let path = root.path(AUTOSTART_RULE);
    let Some(user) = user else {
        match std::fs::remove_file(&path) {
            Ok(()) => return Ok(format!("removed {AUTOSTART_RULE}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok("no autostart rule".into());
            }
            Err(e) => return Err(e).with_context(|| format!("removing {AUTOSTART_RULE}")),
        }
    };
    let rule = autostart_rule(user)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, rule).with_context(|| format!("writing {AUTOSTART_RULE}"))?;
    Ok(format!(
        "wrote {AUTOSTART_RULE}: {user} starts the Windows VM without a prompt"
    ))
}
