//! The boot takeover (todo-80, decided by the user 2026-10-14): systemd-boot and one signed unified kernel image (UKI)
//! for each kernel, beside the boot chain that is there (firmware -> shim -> rEFInd), so Linux starts first and fast.
//! A new firmware entry `legionctl` starts a copy of shim, whose second stage is systemd-boot signed with the key
//! that MOK trusts (the rEFInd key); it is tried once with `BootNext`, and becomes the first entry only after the user
//! confirms a start through it. Nothing of the old chain changes. Its kernels (todo-84): the default, a one-time
//! start, and each kernel's own command line, the one source of that kernel's image.

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
const LOADER: &str = "loader/loader.conf";
/// Each kernel's command line (`<kernel>.cmdline`), the one source of its image's line; `<kernel>.cmdline.old` is the
/// line before the last change, for `kernel cmdline undo`.
pub const LINES: &str = "/etc/legionctl/kernel";
/// Dotted names that the kernel gives to user space (the initramfs, systemd): they are not module parameters.
const USER_SPACE: [&str; 7] = [
    "rd", "systemd", "udev", "luks", "plymouth", "vconsole", "locale",
];

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
    let ukis = images(root);
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

/// legionctl's image of the kernel `name` on the ESP (in `EFI/Linux`).
#[must_use]
pub fn image(name: &str) -> String {
    format!("{UKI_PREFIX}{name}.efi")
}

/// legionctl's images on the ESP.
#[must_use]
pub fn images(root: &SysRoot) -> Vec<String> {
    esp(root)
        .map(|(dir, _)| {
            root.children(Path::new(&dir).join("EFI/Linux"), UKI_PREFIX)
                .iter()
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .collect()
        })
        .unwrap_or_default()
}

fn loader_conf(name: &str) -> String {
    format!(
        "{LOADER_MARK}: the default kernel, no menu (hold a key at start for it)\n\
         default {}\ntimeout 0\neditor no\n",
        image(name)
    )
}

/// The kernel that legionctl's `loader.conf` starts; `None` when the boot takeover is not installed.
#[must_use]
pub fn default_kernel(root: &SysRoot) -> Option<String> {
    let (esp, _) = esp(root)?;
    let conf = root
        .read(Path::new(&esp).join(LOADER))
        .filter(|c| c.starts_with(LOADER_MARK))?;
    conf.lines().find_map(|l| {
        Some(
            l.strip_prefix("default ")?
                .trim()
                .strip_prefix(UKI_PREFIX)?
                .strip_suffix(".efi")?
                .to_string(),
        )
    })
}

fn line_file(name: &str) -> String {
    format!("{LINES}/{name}.cmdline")
}

fn old_file(name: &str) -> String {
    format!("{LINES}/{name}.cmdline.old")
}

/// The command line of the kernel `name`, the source of its image; `None` until the boot takeover writes it.
#[must_use]
pub fn saved_line(root: &SysRoot, name: &str) -> Option<String> {
    root.read(line_file(name))
}

/// Refuse a command line that cannot start the kernel `name` or that the kernel would ignore in part: no `root=`, or
/// a `<module>.<parameter>` for a module that this kernel does not have (built in or loadable, DKMS too). The kernel
/// ignores such a parameter without a word.
///
/// # Errors
/// The line, or the kernel's module tree, fails.
pub fn check_line(root: &SysRoot, name: &str, line: &str) -> Result<()> {
    let words: Vec<&str> = line.split_whitespace().collect();
    anyhow::ensure!(
        words.iter().any(|w| w.starts_with("root=")),
        "the command line of {name} has no root=: the kernel would not find the root file system"
    );
    let release = crate::kernel::release_of(root, name)
        .with_context(|| format!("no module tree of {name} in /usr/lib/modules"))?;
    let modules = crate::kernel::modules(root, &release);
    for word in words {
        let key = word.split('=').next().unwrap_or(word);
        if let Some((module, _)) = key.split_once('.')
            && !USER_SPACE.contains(&module)
            && !modules.contains(&module.replace('-', "_"))
        {
            bail!(
                "{word}: {name} ({release}) has no module {module}, built in or loadable; the kernel would ignore it"
            );
        }
    }
    Ok(())
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

fn mkdir(root: &SysRoot, dir: impl AsRef<Path>) -> Step {
    Step::Run(vec!["mkdir".into(), "-p".into(), text(&root.path(dir))])
}

/// The saved sets with boot entries (`rules sets save --boot`, todo-85).
fn boot_sets(root: &SysRoot) -> Vec<String> {
    crate::ledger::Ledger::open(root)
        .map(|l| l.sets())
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, s)| s.boot)
        .map(|(name, _)| name)
        .collect()
}

/// The image of the kernel `name` with `line`: made beside the old one, signed by `ukify` with the MOK key, its
/// signature checked, and only then put in place of the old one. Each saved set with boot entries is a profile of the
/// image (a multi-profile UKI): its entry starts the kernel with `line` and `legionctl.set=<set>`; profile 0, the
/// default entry, has the kernel's own line.
fn build(root: &SysRoot, esp: &str, name: &str, line: &str) -> Vec<Step> {
    let ucode = ["/boot/intel-ucode.img", "/boot/amd-ucode.img"]
        .into_iter()
        .find(|u| root.exists(u));
    let target = text(&root.path(Path::new(esp).join("EFI/Linux").join(image(name))));
    let new = format!("{target}.new");
    let profiles: Vec<(String, String)> = boot_sets(root)
        .into_iter()
        .map(|set| (format!("{target}.{set}.profile"), set))
        .collect();
    let mut steps: Vec<Step> = profiles
        .iter()
        .map(|(file, set)| {
            Step::Run(vec![
                "ukify".into(),
                "build".into(),
                "--profile".into(),
                format!("ID={set}\nTITLE={name} with {set}"),
                "--cmdline".into(),
                format!("{line} {}{set}", crate::sets::PARAMETER),
                "--output".into(),
                file.clone(),
            ])
        })
        .collect();
    let mut argv: Vec<String> = vec![
        "ukify".into(),
        "build".into(),
        "--linux".into(),
        text(&root.path(format!("/boot/vmlinuz-{name}"))),
    ];
    if let Some(ucode) = ucode {
        argv.extend(["--initrd".into(), text(&root.path(ucode))]);
    }
    argv.extend([
        "--initrd".into(),
        text(&root.path(format!("/boot/initramfs-{name}.img"))),
        "--cmdline".into(),
        line.into(),
        "--secureboot-private-key".into(),
        text(&root.path(KEY)),
        "--secureboot-certificate".into(),
        text(&root.path(CERT)),
        "--output".into(),
        new.clone(),
    ]);
    if !profiles.is_empty() {
        argv.extend(["--profile".into(), format!("TITLE={name}")]);
        for (file, _) in &profiles {
            argv.extend(["--join-profile".into(), file.clone()]);
        }
    }
    steps.extend([
        Step::Run(argv),
        Step::Run(vec![
            "sbverify".into(),
            "--cert".into(),
            text(&root.path(CERT)),
            new.clone(),
        ]),
        Step::Run(vec!["mv".into(), new, target]),
    ]);
    if !profiles.is_empty() {
        steps.push(Step::Run(
            ["rm".to_string(), "-f".into()]
                .into_iter()
                .chain(profiles.into_iter().map(|(file, _)| file))
                .collect(),
        ));
    }
    steps
}

/// The image steps of every kernel in `/boot`, each with its own command line. A kernel without one gets the running
/// system's line, and the plan writes it down as that kernel's.
fn uki_steps(root: &SysRoot, esp: &str) -> Result<Vec<Step>> {
    let names = kernels(root);
    anyhow::ensure!(!names.is_empty(), "no kernel with an initramfs in /boot");
    let mut steps = vec![
        mkdir(root, Path::new(esp).join("EFI/Linux")),
        mkdir(root, LINES),
    ];
    for name in names {
        let saved = saved_line(root, &name);
        let line = if let Some(line) = saved {
            line
        } else {
            let line = cmdline(root)?;
            steps.push(Step::Create {
                path: line_file(&name),
                content: format!("{line}\n"),
            });
            line
        };
        check_line(root, &name, &line)?;
        steps.extend(build(root, esp, &name, &line));
    }
    Ok(steps)
}

/// legionctl's firmware entry and the ESP, or why the boot takeover is not there.
///
/// # Errors
/// The boot takeover is not installed.
pub fn takeover(state: &BootState) -> Result<(String, String)> {
    match (&state.ours, &state.esp) {
        (Some(id), Some(esp)) => Ok((id.clone(), esp.clone())),
        _ => bail!(
            "legionctl's boot entry is not installed: install the boot takeover first (legionctl boot install)"
        ),
    }
}

/// The kernel `name` has a legionctl image to start.
fn startable(state: &BootState, name: &str) -> Result<()> {
    anyhow::ensure!(
        state.kernels.iter().any(|k| k == name),
        "no kernel {name} in /boot (installed: {})",
        state.kernels.join(", ")
    );
    anyhow::ensure!(
        state.ukis.contains(&image(name)),
        "no legionctl image of {name} (legionctl boot rebuild)"
    );
    Ok(())
}

/// Plan `kernel default`: legionctl's `loader.conf` starts `name`.
///
/// # Errors
/// No boot takeover, or no image of `name`.
pub fn default_plan(state: &BootState, name: &str) -> Result<Vec<Step>> {
    let (_, esp) = takeover(state)?;
    startable(state, name)?;
    Ok(vec![Step::Create {
        path: text(&Path::new(&esp).join(LOADER)),
        content: loader_conf(name),
    }])
}

/// Plan `kernel try`: the next start only goes through legionctl's entry (`BootNext`) to `name`
/// (`LoaderEntryOneShot`), then the restart (the last step). The default stays.
///
/// # Errors
/// No boot takeover, or no image of `name`.
pub fn try_plan(state: &BootState, name: &str) -> Result<Vec<Step>> {
    let (id, _) = takeover(state)?;
    startable(state, name)?;
    Ok(vec![
        Step::LoaderVar {
            name: "LoaderEntryOneShot".into(),
            value: image(name),
        },
        Step::Run(vec!["efibootmgr".into(), "--bootnext".into(), id]),
        Step::Run(vec!["systemctl".into(), "reboot".into()]),
    ])
}

/// Plan `kernel cmdline set`: the image of `name` only, with `line` (checked first), then `line` as its command line
/// and the line before it kept for `kernel cmdline undo`. A failed step leaves the old image and line.
///
/// # Errors
/// No boot takeover, no image of `name`, or the line fails [`check_line`].
pub fn cmdline_plan(
    root: &SysRoot,
    state: &BootState,
    name: &str,
    line: &str,
) -> Result<Vec<Step>> {
    let (_, esp) = takeover(state)?;
    startable(state, name)?;
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    check_line(root, name, &line)?;
    let before = saved_line(root, name).with_context(|| {
        format!("no command line of {name} in {LINES} yet (legionctl boot rebuild writes it)")
    })?;
    let mut steps = build(root, &esp, name, &line);
    steps.extend([
        mkdir(root, LINES),
        Step::Create {
            path: old_file(name),
            content: format!("{before}\n"),
        },
        Step::Create {
            path: line_file(name),
            content: format!("{line}\n"),
        },
    ]);
    Ok(steps)
}

/// Plan `kernel cmdline undo`: the line before the last change, as [`cmdline_plan`] (so a second undo goes forward
/// again).
///
/// # Errors
/// No line was changed, or [`cmdline_plan`] fails.
pub fn undo_plan(root: &SysRoot, state: &BootState, name: &str) -> Result<Vec<Step>> {
    let before = root
        .read(old_file(name))
        .with_context(|| format!("no earlier command line of {name} to go back to"))?;
    cmdline_plan(root, state, name, &before)
}

/// Plan `kernel build`'s last step: install the built packages of the kernel `name` (the kernel and its headers,
/// `<name>-<version>-<arch>.pkg.tar.zst` in `dir`) with `pacman -U`. pacman's hooks then make its initramfs, its
/// signed image (`boot rebuild`) and its DKMS modules.
///
/// # Errors
/// `name` is an installed kernel already, or `dir` has no package of it.
pub fn package_plan(root: &SysRoot, name: &str, dir: &Path) -> Result<Vec<Step>> {
    anyhow::ensure!(
        !crate::kernel::installed_packages(root)
            .iter()
            .any(|p| p == name),
        "{name} is an installed kernel already: give the build another --name"
    );
    let mut files: Vec<String> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().into_string().ok()?;
            let rest = file.strip_prefix(name)?.strip_prefix('-')?;
            // the kernel (`<name>-7.2.9-2-...`) and its headers, not the debug package
            ((rest.starts_with(|c: char| c.is_ascii_digit()) || rest.starts_with("headers-"))
                && file.ends_with(".pkg.tar.zst"))
            .then(|| text(&dir.join(file)))
        })
        .collect();
    files.sort();
    anyhow::ensure!(
        files
            .iter()
            .any(|f| !f.contains(&format!("{name}-headers-"))),
        "no package of {name} in {}",
        dir.display()
    );
    Ok(vec![Step::Run(
        ["pacman", "-U", "--noconfirm"]
            .map(String::from)
            .into_iter()
            .chain(files)
            .collect(),
    )])
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
        mkdir(root, &ours),
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
    steps.push(mkdir(root, Path::new(&esp).join("loader")));
    // the running kernel is the default at first
    steps.push(Step::Create {
        path: text(&Path::new(&esp).join(LOADER)),
        content: loader_conf(&running),
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
    let loader = Path::new(&esp).join(LOADER);
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
