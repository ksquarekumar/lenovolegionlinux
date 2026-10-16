//! The installed Windows in a VM (note 05, decided by the user 2026-10-14): the daemon makes the host ready for it and
//! undoes that after. Windows gets its own `NVMe` controller (VFIO); the host must not use it while it runs: no mounted
//! or automounted partition of that disk, no hibernated Windows, no one-time boot into Windows. The GPU moves to and
//! from the running VM on its own (`vfio`). For repairs (todo-151) the VM runs on a copy-on-write overlay instead: the
//! disk is only read (its runner may read the block device), and a commit writes the overlay into it later.

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
    /// The VM runs on an overlay: the disk stays on its driver and its runner may only read it (no VFIO, no memory
    /// lock).
    pub read: bool,
}

/// A device node's owner and mode before a start changed them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Owner {
    pub node: String,
    pub uid: u32,
    pub mode: u32,
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
    /// The disk's block device made readable by the user (an overlay start), as it was before.
    #[serde(default)]
    pub disk_node: Option<Owner>,
}

/// The block device name (`nvme0n1p3`) of a mount source (`/dev/...`, `UUID=...`, `PARTUUID=...`).
fn block(root: &SysRoot, source: &str) -> Result<Option<String>> {
    let link = if let Some((kind, id)) = source.split_once('=') {
        let dir = match kind {
            "UUID" => "uuid",
            "PARTUUID" => "partuuid",
            "LABEL" => "label",
            _ => return Ok(None),
        };
        let Some(link) = root.link(format!("/dev/disk/by-{dir}/{id}"))? else {
            return Ok(None);
        };
        link
    } else {
        PathBuf::from(source)
    };
    Ok(link.file_name().map(|n| n.to_string_lossy().into_owned()))
}

/// The PCI address of the `NVMe` controller of block device `name` (`nvme0n1p3` -> `nvme0`).
fn controller(root: &SysRoot, name: &str) -> Result<Option<String>> {
    let Some(rest) = name.strip_prefix("nvme") else {
        return Ok(None);
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Ok(root
        .link(format!("/sys/class/nvme/nvme{digits}/device"))?
        .and_then(|l| l.file_name().map(|n| n.to_string_lossy().into_owned())))
}

/// The `NVMe` controller of the mount source `source`, if it is on one.
fn source_controller(root: &SysRoot, source: &str) -> Result<Option<String>> {
    match block(root, source)? {
        Some(b) => controller(root, &b),
        None => Ok(None),
    }
}

/// The `NVMe` controller that holds the Windows volume; None when this machine has no Windows volume.
///
/// # Errors
/// The mount table or fstab cannot be read, or the Windows volume is not on an `NVMe` controller.
pub fn windows_disk(root: &SysRoot) -> Result<Option<String>> {
    let Some((source, target)) = windows::volume(root)? else {
        return Ok(None);
    };
    source_controller(root, &source)?
        .with_context(|| {
            format!(
                "the Windows volume {source} ({}) is not on an NVMe controller",
                target.display()
            )
        })
        .map(Some)
}

/// The Windows disk, or an error that says why there is none.
fn require_disk(root: &SysRoot) -> Result<String> {
    windows_disk(root)?.context(
        "no Windows volume: no NTFS entry of the mount table or /etc/fstab holds Windows\\System32\\config\\SYSTEM",
    )
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

/// The block device of the whole Windows disk (`/dev/nvme0n1`): the one namespace of its controller.
///
/// # Errors
/// No Windows disk, or its controller has no namespace or more than one.
pub fn disk_node(root: &SysRoot) -> Result<String> {
    let disk = require_disk(root)?;
    let (_, blocks) = vfio::nodes(root, &disk)?;
    let namespaces: Vec<String> = blocks
        .into_iter()
        .filter(|b| !b.trim_start_matches("/dev/").contains('p'))
        .collect();
    match namespaces.as_slice() {
        [one] => Ok(one.clone()),
        other => bail!(
            "the Windows disk {disk} has {} namespaces, not one: {}",
            other.len(),
            other.join(" ")
        ),
    }
}

/// The disk's size in bytes (the kernel counts 512-byte sectors).
///
/// # Errors
/// See [`disk_node`]; the kernel does not give the size.
pub fn disk_bytes(root: &SysRoot) -> Result<u64> {
    let node = disk_node(root)?;
    let name = node.trim_start_matches("/dev/");
    root.read(format!("/sys/block/{name}/size"))?
        .and_then(|s| s.parse::<u64>().ok())
        .map(|sectors| sectors * 512)
        .with_context(|| format!("/sys/block/{name}/size gives no size"))
}

/// The fstab mount points of NTFS (or any) partitions on the `NVMe` controller `disk`.
fn fstab_targets(root: &SysRoot, disk: &str) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for l in root.read("/etc/fstab")?.unwrap_or_default().lines() {
        if l.trim_start().starts_with('#') {
            continue;
        }
        let mut fields = l.split_whitespace();
        let (Some(source), Some(target)) = (fields.next(), fields.next()) else {
            continue;
        };
        if source_controller(root, source)?.as_deref() == Some(disk) {
            out.push(PathBuf::from(target.replace("\\040", " ")));
        }
    }
    Ok(out)
}

/// The mount points of the Windows disk's partitions in `/etc/fstab`: what `disk_plan` attaches and detaches. None
/// when this machine has no Windows volume.
///
/// # Errors
/// See [`windows_disk`].
pub fn disk_targets(root: &SysRoot) -> Result<Option<Vec<PathBuf>>> {
    windows_disk(root)?
        .map(|disk| fstab_targets(root, &disk))
        .transpose()
}

/// The steps that give the Windows disk's partitions to Linux (`attach`: their automounts started) or take them away
/// (their automounts and mounts stopped), as a VM start does.
///
/// # Errors
/// No Windows disk, or no fstab entry on it.
pub fn disk_plan(root: &SysRoot, attach: bool) -> Result<Vec<Step>> {
    let disk = require_disk(root)?;
    let units: Vec<String> = fstab_targets(root, &disk)?
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
            for d in root.children("/sys/bus/usb/devices", "")? {
                if root.read(d.join("idVendor"))?.as_deref() == Some(vendor)
                    && root.read(d.join("idProduct"))?.as_deref() == Some(product)
                {
                    let number = |f: &str| -> Result<u32> {
                        root.read(d.join(f))?
                            .and_then(|v| v.parse().ok())
                            .with_context(|| format!("USB device {id} has no {f}"))
                    };
                    return Ok(format!(
                        "/dev/bus/usb/{:03}/{:03}",
                        number("busnum")?,
                        number("devnum")?
                    ));
                }
            }
            bail!("no USB device {id} is plugged in (lsusb)")
        })
        .collect()
}

fn chown(root: &SysRoot, node: &str, uid: u32) -> Result<()> {
    rustix::fs::chown(root.path(node), Some(rustix::fs::Uid::from_raw(uid)), None)
        .with_context(|| format!("giving {node} to uid {uid}"))
}

fn chmod(root: &SysRoot, node: &str, mode: u32) -> Result<()> {
    rustix::fs::chmod(root.path(node), rustix::fs::Mode::from_raw_mode(mode))
        .with_context(|| format!("setting the mode of {node} to {mode:o}"))
}

/// Write the overlay `path` (a regular file of `uid`) into the Windows disk with `qemu-img commit`; the disk's
/// automounts stop for the write and start again after. The backing is named here, not taken from the overlay's
/// header, so the write reaches only the Windows disk. A commit that stops half way can run again (it writes the
/// same blocks).
///
/// # Errors
/// A refusal of [`begin`] (a hibernated Windows), the host uses the disk, or `qemu-img` fails.
pub fn commit(root: &SysRoot, path: &str, uid: u32) -> Result<Vec<String>> {
    let why = refusals(root)?;
    if !why.is_empty() {
        bail!(
            "the overlay cannot be written into the disk now:\n- {}",
            why.join("\n- ")
        );
    }
    let overlay = own_file(path, uid)?;
    let disk = require_disk(root)?;
    let node = disk_node(root)?;
    let automounts: Vec<String> = fstab_targets(root, &disk)?
        .iter()
        .map(|t| unit_name(t))
        .collect();
    systemctl(
        "stop",
        &automounts
            .iter()
            .flat_map(|u| [format!("{u}.automount"), format!("{u}.mount")])
            .collect::<Vec<_>>(),
    )?;
    let written = (|| -> Result<Vec<String>> {
        let (users, _) = vfio::users(root, std::slice::from_ref(&disk), true)?;
        if !users.is_empty() {
            bail!("the host uses the Windows disk:\n- {}", users.join("\n- "));
        }
        let fd = std::os::fd::AsRawFd::as_raw_fd(&overlay);
        // the open file, by its number: a name could point at another file by now
        let opts = format!(
            "driver=qcow2,file.filename={},backing.driver=raw,backing.file.filename={}",
            opt(&format!("/proc/{}/fd/{fd}", std::process::id())),
            opt(&root.path(&node).to_string_lossy())
        );
        let out = Command::new("qemu-img")
            .args(["commit", "-d", "--image-opts", &opts])
            .output()
            .context("running qemu-img (package qemu-img)")?;
        anyhow::ensure!(
            out.status.success(),
            "qemu-img commit into {node} failed ({}): {}; the overlay is kept, run the commit again",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
        Ok(vec![format!("the overlay is written into {node}")])
    })();
    let started = systemctl(
        "start",
        &automounts
            .iter()
            .map(|u| format!("{u}.automount"))
            .collect::<Vec<_>>(),
    );
    match (written, started) {
        (Ok(mut lines), Ok(())) => {
            lines.push(format!("started the automounts of {node} again"));
            Ok(lines)
        }
        (Err(e), Ok(())) => Err(e),
        (written, Err(e)) => bail!(
            "{}; starting the automounts again failed: {e:#}",
            written.map_or_else(|w| format!("{w:#}"), |l| l.join("; "))
        ),
    }
}

/// `path` opened for writing when it is a regular file of `uid` (not through a symbolic link).
fn own_file(path: &str, uid: u32) -> Result<std::fs::File> {
    let file = std::fs::File::from(
        rustix::fs::open(
            path,
            rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .with_context(|| format!("opening {path}"))?,
    );
    let meta = file.metadata()?;
    anyhow::ensure!(
        meta.is_file() && std::os::unix::fs::MetadataExt::uid(&meta) == uid,
        "{path} is not a regular file of uid {uid}"
    );
    Ok(file)
}

/// A value for a QEMU option list (a comma is written twice).
fn opt(value: &str) -> String {
    value.replace(',', ",,")
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
    if let Some((_, target)) = windows::volume(root)?
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
    let disk = require_disk(root)?;
    let mut record = Record::default();
    let mut lines = Vec::new();
    let done = (|| -> Result<()> {
        for target in fstab_targets(root, &disk)? {
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
        if ask.read {
            let (users, _) = vfio::users(root, std::slice::from_ref(&disk), true)?;
            if !users.is_empty() {
                bail!("the host uses the Windows disk:\n- {}", users.join("\n- "));
            }
            let node = disk_node(root)?;
            let meta = std::fs::metadata(root.path(&node))
                .with_context(|| format!("reading the owner of {node}"))?;
            record.disk_node = Some(Owner {
                node: node.clone(),
                uid: std::os::unix::fs::MetadataExt::uid(&meta),
                mode: std::os::unix::fs::MetadataExt::mode(&meta) & 0o7777,
            });
            // the owner reads, the group (disk) keeps its access: QEMU opens the disk only as the overlay's backing
            chown(root, &node, uid)?;
            chmod(root, &node, 0o460)?;
            lines.push(format!(
                "{node}: readable by uid {uid} (the VM writes to its overlay)"
            ));
        } else {
            let (_, group) = vfio::group(root, &disk)?;
            lines.extend(vfio::bind(root, &group, uid)?);
            record.devices.extend(group);
        }
        for node in usb_nodes(root, &ask.usb)? {
            chown(root, &node, uid)?;
            lines.push(format!("{node}: uid {uid}"));
            record.usb_nodes.push(node);
        }
        if !ask.read {
            lines.push(vfio::allow_memlock(root, ask.pid, uid, ask.memlock)?);
        }
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
    if let Some(owner) = &record.disk_node {
        let back =
            chown(root, &owner.node, owner.uid).and_then(|()| chmod(root, &owner.node, owner.mode));
        note(format!("{}: owner and mode back", owner.node), back);
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

/// The file of the polkit rule that lets user `user` start and end the Windows VM without a prompt (autostart: at
/// boot no polkit agent can ask, and a prompt at every login is no autostart). Only this action, only this user; one
/// file for each user, so one user's choice does not change another user's.
///
/// # Errors
/// The user name has characters a file name or a rule cannot hold.
pub fn autostart_file(user: &str) -> Result<String> {
    anyhow::ensure!(
        !user.is_empty()
            && !user.starts_with('.')
            && user
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "{user} is not a user name a rule can hold"
    );
    Ok(format!(
        "/etc/polkit-1/rules.d/49-legionctl-windows-vm-{user}.rules"
    ))
}

/// The autostart rule of `user`: no prompt only for a process of the user's local, active session (P-2: never for
/// cron, ssh or a process outside the seat).
///
/// # Errors
/// The user name has characters a rule cannot hold.
pub fn autostart_rule(user: &str) -> Result<String> {
    autostart_file(user)?;
    Ok(format!(
        "// legionctl windows vm autostart: {user} may start and end the Windows VM without a prompt from the active local session\n\
         polkit.addRule(function(action, subject) {{\n    \
         if (action.id == \"org.legionctl.windows.vm\" && subject.user == \"{user}\" && subject.local && subject.active) {{\n        \
         return polkit.Result.YES;\n    }}\n}});\n"
    ))
}

/// Write (`on`) or remove the autostart rule of `user`.
///
/// # Errors
/// The user name has characters a rule cannot hold, or the file cannot be written or removed.
pub fn set_autostart_rule(root: &SysRoot, user: &str, on: bool) -> Result<String> {
    let file = autostart_file(user)?;
    let path = root.path(&file);
    if !on {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(format!("removed {file}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(format!("no autostart rule of {user}"))
            }
            Err(e) => Err(e).with_context(|| format!("removing {file}")),
        };
    }
    let rule = autostart_rule(user)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, rule).with_context(|| format!("writing {file}"))?;
    Ok(format!(
        "wrote {file}: {user} starts the Windows VM without a prompt"
    ))
}
