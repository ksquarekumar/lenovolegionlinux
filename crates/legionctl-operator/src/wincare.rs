//! Windows care (todo-151, note 05): repairs run in the Windows VM on a copy-on-write overlay of the Windows disk
//! (commit or drop after), through the QEMU guest agent while Windows runs, or typed into Windows RE; restore points; a
//! full image on request; and the host actions that Windows asks for on its channel.

use std::io::Read;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use blake2::{Blake2b512, Digest};
use legionctl_api::{HostAction, Lines, OverlayAction, OverlayDisk, RepairStep};
use legionctl_core::db::Db;
use legionctl_core::qga::Qga;
use legionctl_machine::sysroot::SysRoot;
use legionctl_machine::{windows, winvm};
use serde_json::json;

use crate::engine::{Operator, output, unit_active, windows_qmp};

/// The job of a repair.
pub(crate) const REPAIR: &str = "windows.repair";

fn dir() -> PathBuf {
    legionctl_core::session::windows_root()
}

/// The overlay that takes Windows' writes while it exists.
pub(crate) fn overlay_file() -> PathBuf {
    dir().join("overlay.qcow2")
}

/// The disk's stamp when the overlay was made.
fn stamp_file() -> PathBuf {
    dir().join("overlay.stamp")
}

/// What Windows last wrote on its disk: the hash of its SYSTEM hive (Windows writes it at each start and shut
/// down). shortcut: one file, not the whole disk; a change that leaves the hive as it was is not seen.
fn stamp(root: &SysRoot) -> Result<String> {
    let hive = windows::system_hive(root)?.context(
        "no Windows volume: no NTFS entry of the mount table or /etc/fstab holds Windows\\System32\\config\\SYSTEM",
    )?;
    let bytes = std::fs::read(&hive).with_context(|| format!("reading {}", hive.display()))?;
    Ok(format!("{:x}", Blake2b512::digest(bytes)))
}

/// Refuse when Windows ran on the disk since the overlay was made: the overlay's blocks do not fit the disk now.
fn check_stamp(root: &SysRoot) -> Result<()> {
    let made = std::fs::read_to_string(stamp_file())
        .with_context(|| format!("reading {}", stamp_file().display()))?;
    ensure!(
        made.trim() == stamp(root)?,
        "Windows ran on its disk since the overlay was made (its SYSTEM hive changed): the overlay does not fit the \
         disk now; drop it (legionctl windows vm overlay drop)"
    );
    Ok(())
}

fn vm_runs() -> Result<bool> {
    unit_active(&format!(
        "{}.service",
        legionctl_core::session::WINDOWS_UNIT
    ))
}

fn stopped() -> Result<()> {
    ensure!(
        !vm_runs()?,
        "the Windows VM runs: stop it first (legionctl windows vm stop)"
    );
    Ok(())
}

/// Repairs change Windows only on an overlay of a running VM.
fn on_overlay() -> Result<()> {
    ensure!(
        overlay_file().exists(),
        "a repair runs only on an overlay: legionctl windows vm stop, windows vm overlay create, windows vm start"
    );
    ensure!(
        vm_runs()?,
        "the Windows VM does not run (legionctl windows vm start)"
    );
    Ok(())
}

fn agent() -> Result<Qga> {
    Qga::connect(&dir().join("qga.sock"))
}

fn cmd(command: &str) -> impl FnOnce(&str) -> Vec<String> {
    let command = command.to_string();
    move |log| vec!["/c".into(), format!("{command} > {log} 2>&1")]
}

fn powershell(script: &str) -> impl FnOnce(&str) -> Vec<String> {
    let script = script.to_string();
    move |log| {
        vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            format!("{script} 2>&1 | Set-Content -Encoding UTF8 -Path '{log}'"),
        ]
    }
}

/// The last lines of `text`, and how many were left out.
fn tail(text: &str, keep: usize) -> Vec<String> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let skip = lines.len().saturating_sub(keep);
    let mut out: Vec<String> = Vec::new();
    if skip > 0 {
        out.push(format!(
            "({skip} lines left out: all are in the windows.repair.output events, legionctl events)"
        ));
    }
    out.extend(lines[skip..].iter().map(|l| l.trim_end().to_string()));
    out
}

/// What a repair's output and exit code say: done, or a failure with the reason.
fn verdict(step: RepairStep, code: i64, text: &str) -> Result<String> {
    Ok(match step {
        RepairStep::Sfc if text.contains("did not find any integrity violations") => {
            "sfc: no integrity violations".into()
        }
        RepairStep::Sfc if text.contains("successfully repaired") => {
            "sfc: found corrupt files and repaired them".into()
        }
        RepairStep::Sfc => bail!(
            "sfc did not repair Windows (exit {code}): see C:\\Windows\\Logs\\CBS\\CBS.log; run dism, then sfc again"
        ),
        RepairStep::Dism if code == 0 => "DISM: the component store is healthy now".into(),
        RepairStep::Dism => bail!(
            "DISM failed (exit {code}): see C:\\Windows\\Logs\\DISM\\dism.log (it needs Windows Update)"
        ),
        RepairStep::Chkdsk if code == 0 => "chkdsk: no problems found".into(),
        RepairStep::Chkdsk if code == 1 => "chkdsk: problems found and fixed".into(),
        RepairStep::Chkdsk => bail!(
            "chkdsk found problems it cannot fix while Windows runs (exit {code}): start WinRE (windows vm \
             recovery) and run chkdsk C: /f there"
        ),
        RepairStep::Boot => bail!("boot repair runs in WinRE, not through the guest agent"),
    })
}

/// The command that boot repair types into Windows RE's command prompt: `bcdboot` for the volume that holds Windows
/// (its letter differs in Windows RE; X: is Windows RE itself), onto the EFI partition the firmware starts from.
const BCDBOOT: &str = "for %d in (C D E F G H I J K L M N O P Q R S T U V W Y Z) do @if exist \
                       %d:\\Windows\\System32\\config\\SYSTEM bcdboot %d:\\Windows /f UEFI";

/// QMP key codes for the characters of [`BCDBOOT`] (a US keyboard in Windows RE): (shift, key).
fn key(c: char) -> Result<(bool, String)> {
    Ok(match c {
        'a'..='z' | '0'..='9' => (false, c.to_string()),
        'A'..='Z' => (true, c.to_ascii_lowercase().to_string()),
        ' ' => (false, "spc".into()),
        '\\' => (false, "backslash".into()),
        '.' => (false, "dot".into()),
        '/' => (false, "slash".into()),
        '-' => (false, "minus".into()),
        ':' => (true, "semicolon".into()),
        '%' => (true, "5".into()),
        '@' => (true, "2".into()),
        '(' => (true, "9".into()),
        ')' => (true, "0".into()),
        other => bail!("no key for {other:?}"),
    })
}

/// Type `text` and Enter into the VM's console.
fn type_text(text: &str) -> Result<()> {
    let mut qmp = windows_qmp()?;
    for c in text.chars() {
        let (shift, code) = key(c)?;
        let mut keys = Vec::new();
        if shift {
            keys.push(json!({"type": "qcode", "data": "shift"}));
        }
        keys.push(json!({"type": "qcode", "data": code}));
        qmp.execute("send-key", Some(json!({ "keys": keys })))?;
    }
    qmp.execute(
        "send-key",
        Some(json!({"keys": [{"type": "qcode", "data": "ret"}]})),
    )?;
    Ok(())
}

/// Boot repair: [`BCDBOOT`] typed into Windows RE's open command prompt.
fn boot_repair() -> Result<Lines> {
    // WinRE has no guest agent: an answer means Windows runs, and the keys would go to its desktop
    if agent().is_ok() {
        bail!(
            "Windows runs, not WinRE: legionctl windows vm recovery first, then open Troubleshoot > Command Prompt \
             in its console (windows vm view)"
        );
    }
    type_text(BCDBOOT)?;
    Ok(Lines {
        lines: vec![
            format!("typed into the VM's console: {BCDBOOT}"),
            "bcdboot says \"Boot files successfully created\" in the console (windows vm view); then close WinRE \
             (Continue) and check that Windows starts"
                .into(),
        ],
    })
}

/// The restore points as `number|date|description` lines.
fn restore_points(qga: &mut Qga) -> Result<Vec<String>> {
    let (code, text) = qga.run(
        "powershell.exe",
        powershell(
            "Get-ComputerRestorePoint | ForEach-Object { '{0}|{1}|{2}' -f $_.SequenceNumber, \
             $_.ConvertToDateTime($_.CreationTime).ToString('yyyy-MM-dd HH:mm'), $_.Description }",
        ),
        |_| Ok(()),
    )?;
    ensure!(
        code == 0,
        "Get-ComputerRestorePoint failed (exit {code}): {}",
        text.trim()
    );
    Ok(text
        .lines()
        .filter(|l| l.contains('|'))
        .map(String::from)
        .collect())
}

/// `node` into the new qcow2 file `target`; the line with its size and hash.
fn image(node: &std::path::Path, target: &std::path::Path) -> Result<String> {
    output(
        Command::new("qemu-img")
            .args(["convert", "-O", "qcow2"])
            .arg(node)
            .arg(target),
    )?;
    let mut file = std::fs::File::open(target)?;
    let mut hash = Blake2b512::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!(
        "image {}: {} bytes, blake2b-512 {:x}",
        target.display(),
        file.metadata()?.len(),
        hash.finalize()
    ))
}

impl Operator {
    /// `windows vm overlay`.
    pub(crate) fn windows_overlay(&self, action: OverlayAction) -> Result<Lines> {
        let root = SysRoot::from_env();
        let file = overlay_file();
        let lines = match action {
            OverlayAction::Status if !file.exists() => {
                vec!["no overlay: the VM starts on the Windows disk itself".into()]
            }
            OverlayAction::Status => {
                let fits = match check_stamp(&root) {
                    Ok(()) => "Windows did not run on the disk since".to_string(),
                    Err(e) => format!("{e:#}"),
                };
                vec![
                    format!(
                        "overlay {} ({} MiB of changes): each VM start runs on it",
                        file.display(),
                        std::fs::metadata(&file)?.len() >> 20
                    ),
                    fits,
                ]
            }
            OverlayAction::Create => {
                stopped()?;
                ensure!(
                    !file.exists(),
                    "there is an overlay already (legionctl windows vm overlay status)"
                );
                let bytes = winvm::disk_bytes(&root)?;
                let node = winvm::disk_node(&root)?;
                std::fs::create_dir_all(dir())?;
                let stamp = stamp(&root)?;
                output(
                    Command::new("qemu-img")
                        .args(["create", "-q", "-f", "qcow2"])
                        .arg(&file)
                        .arg(bytes.to_string()),
                )?;
                std::fs::write(stamp_file(), stamp)?;
                vec![format!(
                    "made {} over {node}: each VM start writes to it and {node} stays as it is, until commit or drop",
                    file.display()
                )]
            }
            OverlayAction::Drop => {
                stopped()?;
                ensure!(file.exists(), "there is no overlay");
                std::fs::remove_file(&file)?;
                std::fs::remove_file(stamp_file())?;
                vec![format!(
                    "removed {}: the Windows disk stays as it was",
                    file.display()
                )]
            }
            OverlayAction::Commit => {
                stopped()?;
                ensure!(file.exists(), "there is no overlay");
                check_stamp(&root)?;
                let path = file.to_string_lossy().into_owned();
                let mut lines =
                    self.system(async move |p| p.windows_overlay_commit(&path).await)?;
                std::fs::remove_file(&file)?;
                std::fs::remove_file(stamp_file())?;
                lines.push(format!("removed {}", file.display()));
                lines
            }
        };
        Ok(Lines { lines })
    }

    /// `windows vm image`: the disk, only read, into a new qcow2 file, and the file's hash.
    pub(crate) fn windows_image(&self, path: &str) -> Result<Lines> {
        stopped()?;
        let target = PathBuf::from(path);
        ensure!(
            !target.exists(),
            "{path} exists: give a new file for the image"
        );
        let root = SysRoot::from_env();
        let node = root.path(winvm::disk_node(&root)?);
        let db = Db::open_default()?;
        let job = db.job_begin("windows.image", &json!({ "path": path }))?;
        let out = (|| -> Result<Lines> {
            let mut lines = self.system(async |p| p.windows_overlay_begin(&[]).await)?.0;
            let made = image(&node, &target);
            let back = self.system(async |p| p.windows_vm_end().await);
            let line = match (made, back) {
                (Ok(line), Ok(back)) => {
                    lines.extend(back);
                    line
                }
                (Err(e), Ok(_)) => return Err(e),
                (made, Err(e)) => bail!(
                    "{}; giving the disk back failed: {e:#} (legionctl windows vm stop)",
                    made.unwrap_or_else(|m| format!("{m:#}"))
                ),
            };
            lines.push(line);
            Ok(Lines { lines })
        })();
        crate::engine::end_job(&db, job, &out)?;
        out
    }

    /// `internal.windows_host`: a host action that Windows asked for; the user allows it on the Linux desktop, then
    /// the host's own approval applies (logind for shut down and restart, polkit for the restart into Windows).
    pub(crate) fn windows_host(&self, action: HostAction) -> Result<Lines> {
        let what = match action {
            HostAction::Shutdown => "shut down this whole machine (Linux and the Windows VM)",
            HostAction::Restart => "restart this whole machine into Linux",
            HostAction::RestartWindows => {
                ensure!(
                    !overlay_file().exists(),
                    "an overlay holds repair changes: Windows on the machine itself would change the disk under \
                     it; commit or drop it first (legionctl windows vm overlay)"
                );
                "restart this whole machine into Windows on the machine itself (bare metal)"
            }
        };
        let allowed = self
            .runtime
            .block_on(crate::desktop::ask(
                "legionctl: Windows in the VM asks",
                &format!("Windows in the VM asks to {what}. Allow?"),
            ))
            .context("no desktop to ask the user")?;
        ensure!(allowed, "the user did not allow it on the Linux desktop");
        let lines = match action {
            HostAction::Shutdown => {
                output(Command::new("systemctl").arg("poweroff"))?;
                vec!["the machine shuts down".into()]
            }
            HostAction::Restart => {
                output(Command::new("systemctl").arg("reboot"))?;
                vec!["the machine restarts into Linux".into()]
            }
            HostAction::RestartWindows => self.system(async |p| p.reboot_windows().await)?,
        };
        Ok(Lines { lines })
    }
}

/// `windows vm recovery`: Windows RE at the next start of Windows, then a restart.
pub(crate) fn recovery() -> Result<Lines> {
    on_overlay()?;
    let mut qga = agent()?;
    let (code, text) = qga.run("cmd.exe", cmd("reagentc /boottore"), |_| Ok(()))?;
    ensure!(
        code == 0,
        "WinRE cannot start (reagentc /boottore, exit {code}): {}; Windows also starts WinRE by itself after two \
         failed starts",
        text.trim()
    );
    qga.spawn("shutdown.exe", &["/r".into(), "/t".into(), "5".into()])?;
    Ok(Lines {
        lines: vec![
            text.trim().to_string(),
            "Windows restarts into WinRE in 5 s: legionctl windows vm view shows its console"
                .into(),
        ],
    })
}

/// `windows repair`: a repair step, only on an overlay.
pub(crate) fn repair(step: RepairStep) -> Result<Lines> {
    on_overlay()?;
    let command = match step {
        RepairStep::Sfc => "sfc /scannow",
        RepairStep::Dism => "DISM /Online /Cleanup-Image /RestoreHealth",
        RepairStep::Chkdsk => "chkdsk C: /scan",
        RepairStep::Boot => return boot_repair(),
    };
    let mut qga = agent()?;
    let db = Db::open_default()?;
    let job = db.job_begin(REPAIR, &json!({ "step": step }))?;
    let out = qga
        .run("cmd.exe", cmd(command), |text| {
            db.record(
                "operator",
                "windows.repair.output",
                &json!({"job": job, "step": step, "text": text}),
            )
            .map(|_| ())
        })
        .and_then(|(code, text)| {
            let verdict =
                verdict(step, code, &text).map_err(|e| e.context(tail(&text, 10).join("\n")))?;
            let mut lines = vec![format!("{command}: {verdict} (job {job})")];
            lines.extend(tail(&text, 10));
            Ok(Lines { lines })
        });
    crate::engine::end_job(&db, job, &out)?;
    out
}

/// `windows restore list`.
pub(crate) fn restore_list() -> Result<Lines> {
    let points = restore_points(&mut agent()?)?;
    let lines = if points.is_empty() {
        vec!["no restore points (System Protection is off for C:, or none was made)".into()]
    } else {
        points.iter().map(|p| p.replace('|', "  ")).collect()
    };
    Ok(Lines { lines })
}

/// `windows restore apply`: only on an overlay; Windows restarts to restore the point.
pub(crate) fn restore_apply(point: u32) -> Result<Lines> {
    on_overlay()?;
    let mut qga = agent()?;
    let points = restore_points(&mut qga)?;
    ensure!(
        points
            .iter()
            .any(|p| p.split('|').next() == Some(&point.to_string())),
        "no restore point {point} (legionctl windows restore list)"
    );
    let pid = qga.spawn(
        "powershell.exe",
        &[
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            format!("Restore-Computer -RestorePoint {point} -Confirm:$false"),
        ],
    )?;
    // Windows restarts to restore: the agent stops answering; an exit before that is a failure
    for _ in 0..30 {
        match qga.exited(pid) {
            Ok(Some(0)) => break,
            Ok(Some(code)) => bail!("Restore-Computer failed (exit {code})"),
            Ok(None) => std::thread::sleep(std::time::Duration::from_secs(1)),
            Err(e) => {
                return Ok(Lines {
                    lines: vec![format!(
                        "Windows restarts to restore point {point} (the guest agent stopped answering: {e:#}); \
                         legionctl windows vm view shows it"
                    )],
                });
            }
        }
    }
    Ok(Lines {
        lines: vec![format!(
            "Windows restores point {point} and restarts; legionctl windows vm view shows it"
        )],
    })
}

/// The overlay for a VM start, after its stamp check; None when there is none.
pub(crate) fn start_overlay(root: &SysRoot) -> Result<Option<OverlayDisk>> {
    let file = overlay_file();
    if !file.exists() {
        return Ok(None);
    }
    check_stamp(root)?;
    Ok(Some(OverlayDisk {
        file: file.to_string_lossy().into_owned(),
        disk: root
            .path(winvm::disk_node(root)?)
            .to_string_lossy()
            .into_owned(),
    }))
}
