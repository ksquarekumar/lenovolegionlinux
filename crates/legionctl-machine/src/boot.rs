//! The boot takeover (todo-80, decided by the user 2026-10-14): systemd-boot and one signed unified kernel image (UKI)
//! for each kernel, beside the boot chain that is there (firmware -> shim -> rEFInd), so Linux starts first and fast.
//! A new firmware entry `legionctl` starts a copy of shim, whose second stage is systemd-boot signed with the key
//! that MOK trusts (the rEFInd key); it is tried once with `BootNext`, and becomes the first entry only after the user
//! confirms a start through it. Nothing of the old chain changes.

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::policy::Step;
use crate::sysroot::SysRoot;

/// The firmware entry's label, and our folder on the ESP.
pub const LABEL: &str = "legionctl";
const DIR: &str = "EFI/legionctl";
/// The key and certificate that MOK trusts (rEFInd's local key).
pub const KEY: &str = "/etc/refind.d/keys/refind_local.key";
pub const CERT: &str = "/etc/refind.d/keys/refind_local.crt";
const SYSTEMD_BOOT: &str = "/usr/lib/systemd/boot/efi/systemd-bootx64.efi";
/// Our UKIs on the ESP: `EFI/Linux/legionctl-<kernel>.efi`.
const UKI_PREFIX: &str = "legionctl-";
const LOADER_MARK: &str = "# legionctl";

/// A firmware boot entry.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct Entry {
    /// `0003`
    pub id: String,
    pub label: String,
}

/// What the firmware and the ESP have now.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct BootState {
    /// The ESP's mount point (`/boot/efi`).
    pub esp: Option<String>,
    pub entries: Vec<Entry>,
    pub order: Vec<String>,
    pub current: Option<String>,
    pub next: Option<String>,
    /// The `legionctl` entry, when it exists.
    pub ours: Option<String>,
    /// Kernels in `/boot` (`linux-cachyos`), and which have our UKI.
    pub kernels: Vec<String>,
    pub ukis: Vec<String>,
}

/// `efibootmgr` output: the entries, the boot order, `BootCurrent` and `BootNext`.
#[must_use]
pub fn parse_efibootmgr(text: &str) -> (Vec<Entry>, Vec<String>, Option<String>, Option<String>) {
    let mut entries = Vec::new();
    let (mut order, mut current, mut next) = (Vec::new(), None, None);
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("BootOrder: ") {
            order = v.split(',').map(|s| s.trim().to_string()).collect();
        } else if let Some(v) = line.strip_prefix("BootCurrent: ") {
            current = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("BootNext: ") {
            next = Some(v.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("Boot")
            && let Some((id, label)) = rest.split_at_checked(4)
            && id.bytes().all(|b| b.is_ascii_hexdigit())
        {
            let label = label.trim_start_matches('*').trim();
            let label = label.split('\t').next().unwrap_or(label).trim();
            entries.push(Entry {
                id: id.to_string(),
                label: label.to_string(),
            });
        }
    }
    (entries, order, current, next)
}

fn efibootmgr(args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("efibootmgr")
        .args(args)
        .output()
        .context("running efibootmgr")?;
    anyhow::ensure!(
        out.status.success(),
        "efibootmgr {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The ESP's mount point and its partition's device (`/dev/nvme1n1p1`).
fn esp(root: &SysRoot) -> Option<(String, String)> {
    root.read("/proc/self/mounts")?.lines().find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        (f.len() > 2 && f[2] == "vfat" && ["/boot/efi", "/efi", "/boot"].contains(&f[1]))
            .then(|| (f[1].to_string(), f[0].to_string()))
    })
}

/// Kernels with an initramfs in `/boot`: `vmlinuz-<name>` and `initramfs-<name>.img`.
fn kernels(root: &SysRoot) -> Vec<String> {
    root.children("/boot", "vmlinuz-")
        .iter()
        .filter_map(|p| {
            let name = p
                .file_name()?
                .to_string_lossy()
                .strip_prefix("vmlinuz-")?
                .to_string();
            root.exists(format!("/boot/initramfs-{name}.img"))
                .then_some(name)
        })
        .collect()
}

/// The current state.
///
/// # Errors
/// `efibootmgr` fails (no EFI variables).
pub fn state(root: &SysRoot) -> Result<BootState> {
    let (entries, order, current, next) = parse_efibootmgr(&efibootmgr(&[])?);
    let esp_dir = esp(root).map(|(dir, _)| dir);
    let ukis = esp_dir
        .as_ref()
        .map(|dir| {
            root.children(Path::new(dir).join("EFI/Linux"), UKI_PREFIX)
                .iter()
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .collect()
        })
        .unwrap_or_default();
    Ok(BootState {
        ours: entries
            .iter()
            .find(|e| e.label == LABEL)
            .map(|e| e.id.clone()),
        esp: esp_dir,
        entries,
        order,
        current,
        next,
        kernels: kernels(root),
        ukis,
    })
}

/// The running system's kernel line, without what the boot manager adds (`initrd=`, `BOOT_IMAGE=`).
fn cmdline(root: &SysRoot) -> Result<String> {
    let line = root
        .read("/proc/cmdline")
        .context("reading /proc/cmdline")?;
    Ok(line
        .split_whitespace()
        .filter(|w| !w.starts_with("initrd=") && !w.starts_with("BOOT_IMAGE="))
        .collect::<Vec<_>>()
        .join(" "))
}

/// The running kernel's name in `/boot` (`linux-cachyos-bore-lto`), from the `initrd=` the boot manager passed.
fn running_kernel(root: &SysRoot) -> Result<String> {
    let line = root
        .read("/proc/cmdline")
        .context("reading /proc/cmdline")?;
    line.split_whitespace()
        .find_map(|w| {
            let image = w.strip_prefix("initrd=")?.rsplit(['\\', '/']).next()?;
            Some(
                image
                    .strip_prefix("initramfs-")?
                    .strip_suffix(".img")?
                    .to_string(),
            )
        })
        .or_else(|| kernels(root).into_iter().next())
        .context("no running kernel in /proc/cmdline and no kernel in /boot")
}

fn text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The UKI steps of every kernel in `/boot`, each signed by `ukify` with the MOK key.
fn uki_steps(root: &SysRoot, esp: &str) -> Result<Vec<Step>> {
    let line = cmdline(root)?;
    let ucode = ["/boot/intel-ucode.img", "/boot/amd-ucode.img"]
        .into_iter()
        .find(|u| root.exists(u));
    let names = kernels(root);
    anyhow::ensure!(!names.is_empty(), "no kernel with an initramfs in /boot");
    let linux = Path::new(esp).join("EFI/Linux");
    let mut steps = vec![Step::Run(vec![
        "mkdir".into(),
        "-p".into(),
        text(&root.path(&linux)),
    ])];
    for name in names {
        let mut argv: Vec<String> = vec!["ukify".into(), "build".into()];
        argv.extend([
            "--linux".into(),
            text(&root.path(format!("/boot/vmlinuz-{name}"))),
        ]);
        if let Some(ucode) = ucode {
            argv.extend(["--initrd".into(), text(&root.path(ucode))]);
        }
        argv.extend([
            "--initrd".into(),
            text(&root.path(format!("/boot/initramfs-{name}.img"))),
            "--cmdline".into(),
            line.clone(),
            "--secureboot-private-key".into(),
            text(&root.path(KEY)),
            "--secureboot-certificate".into(),
            text(&root.path(CERT)),
            "--output".into(),
            text(&root.path(linux.join(format!("{UKI_PREFIX}{name}.efi")))),
        ]);
        steps.push(Step::Run(argv));
    }
    Ok(steps)
}

/// Plan `boot install`: our shim copy with systemd-boot as its signed second stage, the UKIs, `loader.conf` (the
/// running kernel first, no wait), and a new firmware entry that is not first in the order.
///
/// # Errors
/// No ESP, no shim on it, no MOK key, no systemd-boot, no kernels.
pub fn install_plan(root: &SysRoot) -> Result<Vec<Step>> {
    let running = running_kernel(root)?;
    let (esp, device) =
        esp(root).context("no ESP is mounted at /boot/efi, /efi or /boot (vfat)")?;
    let shim = Path::new(&esp).join("EFI/BOOT/bootx64.efi");
    let manager = Path::new(&esp).join("EFI/BOOT/mmx64.efi");
    if !root.exists(&shim) || !root.exists(&manager) {
        bail!(
            "no shim on the ESP ({} and mmx64.efi): legionctl adds systemd-boot only beside a shim chain",
            shim.display()
        );
    }
    for need in [KEY, CERT, SYSTEMD_BOOT] {
        anyhow::ensure!(root.exists(need), "{need} is missing");
    }
    let ours = Path::new(&esp).join(DIR);
    let mut steps = vec![
        Step::Run(vec!["mkdir".into(), "-p".into(), text(&root.path(&ours))]),
        Step::Run(vec![
            "install".into(),
            "-m".into(),
            "644".into(),
            text(&root.path(&shim)),
            text(&root.path(ours.join("shimx64.efi"))),
        ]),
        Step::Run(vec![
            "install".into(),
            "-m".into(),
            "644".into(),
            text(&root.path(&manager)),
            text(&root.path(ours.join("mmx64.efi"))),
        ]),
        // shim starts grubx64.efi from its own folder: systemd-boot, signed with the key MOK trusts
        Step::Run(vec![
            "sbsign".into(),
            "--key".into(),
            text(&root.path(KEY)),
            "--cert".into(),
            text(&root.path(CERT)),
            "--output".into(),
            text(&root.path(ours.join("grubx64.efi"))),
            text(&root.path(SYSTEMD_BOOT)),
        ]),
    ];
    steps.extend(uki_steps(root, &esp)?);
    steps.push(Step::Run(vec![
        "mkdir".into(),
        "-p".into(),
        text(&root.path(Path::new(&esp).join("loader"))),
    ]));
    steps.push(Step::Create {
        path: text(&Path::new(&esp).join("loader/loader.conf")),
        content: format!(
            "{LOADER_MARK}: the running kernel first, no menu (hold a key at start for it)\n\
             default {UKI_PREFIX}{running}.efi\ntimeout 0\neditor no\n"
        ),
    });
    let partition = device.trim_start_matches("/dev/");
    let number = root
        .read(format!("/sys/class/block/{partition}/partition"))
        .context("the ESP's partition number")?;
    // the partition's sysfs folder is inside its disk's: .../nvme1n1/nvme1n1p1
    let disk = root
        .link(format!("/sys/class/block/{partition}"))
        .and_then(|p| {
            p.parent()?
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .context("the ESP's disk")?;
    steps.push(Step::Run(vec![
        "efibootmgr".into(),
        "--create-only".into(),
        "--disk".into(),
        format!("/dev/{disk}"),
        "--part".into(),
        number,
        "--label".into(),
        LABEL.into(),
        "--loader".into(),
        format!("\\{}\\shimx64.efi", DIR.replace('/', "\\")),
    ]));
    Ok(steps)
}

/// The step that tries our entry once at the next start.
///
/// # Errors
/// Our entry does not exist.
pub fn try_once(state: &BootState) -> Result<Step> {
    let id = state
        .ours
        .clone()
        .context("no legionctl boot entry (legionctl boot install)")?;
    Ok(Step::Run(vec![
        "efibootmgr".into(),
        "--bootnext".into(),
        id,
    ]))
}

/// Plan `boot confirm`: our entry first in the order, after a start through it.
///
/// # Errors
/// No entry, or this start did not come through it.
pub fn confirm_plan(state: &BootState) -> Result<Vec<Step>> {
    let id = state
        .ours
        .clone()
        .context("no legionctl boot entry (legionctl boot install)")?;
    if state.current.as_deref() != Some(id.as_str()) {
        bail!(
            "this start came through entry {}, not legionctl ({id}): restart once through it (it is the next start's \
             entry after boot install), then confirm",
            state.current.as_deref().unwrap_or("unknown")
        );
    }
    let order: Vec<String> = std::iter::once(id.clone())
        .chain(state.order.iter().filter(|e| **e != id).cloned())
        .collect();
    Ok(vec![Step::Run(vec![
        "efibootmgr".into(),
        "--bootorder".into(),
        order.join(","),
    ])])
}

/// Plan `boot rebuild` (the pacman hook): every kernel's UKI again, and the UKIs of kernels that are gone removed.
///
/// # Errors
/// No ESP or no kernels.
pub fn rebuild_plan(root: &SysRoot, state: &BootState) -> Result<Vec<Step>> {
    let esp = state.esp.clone().context("no ESP is mounted")?;
    let mut steps = uki_steps(root, &esp)?;
    for uki in &state.ukis {
        let kernel = uki.trim_start_matches(UKI_PREFIX).trim_end_matches(".efi");
        if !state.kernels.iter().any(|k| k == kernel) {
            steps.push(Step::Remove {
                path: text(&Path::new(&esp).join("EFI/Linux").join(uki)),
            });
        }
    }
    Ok(steps)
}

/// Plan `boot remove`: our entry, our folder, our UKIs and our `loader.conf` gone; the old chain as it was.
///
/// # Errors
/// No ESP.
pub fn remove_plan(root: &SysRoot, state: &BootState) -> Result<Vec<Step>> {
    let esp = state.esp.clone().context("no ESP is mounted")?;
    let mut steps = Vec::new();
    if let Some(id) = &state.ours {
        steps.push(Step::Run(vec![
            "efibootmgr".into(),
            "--delete-bootnum".into(),
            "--bootnum".into(),
            id.clone(),
        ]));
    }
    steps.push(Step::Run(vec![
        "rm".into(),
        "-rf".into(),
        text(&root.path(Path::new(&esp).join(DIR))),
    ]));
    steps.extend(state.ukis.iter().map(|uki| Step::Remove {
        path: text(&Path::new(&esp).join("EFI/Linux").join(uki)),
    }));
    let loader = Path::new(&esp).join("loader/loader.conf");
    if root
        .read(&loader)
        .is_some_and(|c| c.starts_with(LOADER_MARK))
    {
        steps.push(Step::Remove {
            path: text(&loader),
        });
    }
    Ok(steps)
}
