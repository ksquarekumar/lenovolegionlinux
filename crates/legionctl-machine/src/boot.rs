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
        .context("running efibootmgr (install the package efibootmgr)")?;
    anyhow::ensure!(
        out.status.success(),
        "efibootmgr {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The ESP's mount point and its partition's device (`/dev/nvme1n1p1`). An automount (`x-systemd.automount`, or
/// the ESP that systemd-gpt-auto-generator gives) is `autofs` in the mount table until a program opens the place, so
/// each place is opened first.
fn esp(root: &SysRoot) -> Result<Option<(String, String)>> {
    const PLACES: [&str; 3] = ["/boot/efi", "/efi", "/boot"];
    for place in PLACES {
        // the open mounts an automount; whether the place is there shows in the mount table
        drop(std::fs::read_dir(root.path(place)));
    }
    Ok(root
        .read("/proc/self/mounts")?
        .unwrap_or_default()
        .lines()
        .find_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() > 2 && f[2] == "vfat" && PLACES.contains(&f[1]))
                .then(|| (f[1].to_string(), f[0].to_string()))
        }))
}

/// Kernels with an initramfs in `/boot`: `vmlinuz-<name>` and `initramfs-<name>.img`.
fn kernels(root: &SysRoot) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for p in root.children("/boot", "vmlinuz-")? {
        let Some(name) = p.file_name().and_then(|n| {
            n.to_string_lossy()
                .strip_prefix("vmlinuz-")
                .map(String::from)
        }) else {
            continue;
        };
        if root.exists(format!("/boot/initramfs-{name}.img"))? {
            out.push(name);
        }
    }
    Ok(out)
}

/// The current state.
///
/// # Errors
/// `efibootmgr` fails (no EFI variables), or `/boot` or the ESP cannot be read.
pub fn state(root: &SysRoot) -> Result<BootState> {
    let (entries, order, current, next) = parse_efibootmgr(&efibootmgr(&[])?);
    let esp_dir = esp(root)?.map(|(dir, _)| dir);
    let ukis = images(root)?;
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
        kernels: kernels(root)?,
        ukis,
    })
}

/// legionctl's image of the kernel `name` on the ESP (in `EFI/Linux`).
#[must_use]
pub fn image(name: &str) -> String {
    format!("{UKI_PREFIX}{name}.efi")
}

/// legionctl's images on the ESP.
///
/// # Errors
/// The mount table or the ESP cannot be read.
pub fn images(root: &SysRoot) -> Result<Vec<String>> {
    let Some((dir, _)) = esp(root)? else {
        return Ok(Vec::new());
    };
    Ok(root
        .children(Path::new(&dir).join("EFI/Linux"), UKI_PREFIX)?
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect())
}

fn loader_conf(name: &str) -> String {
    format!(
        "{LOADER_MARK}: the default kernel, no menu (hold a key at start for it)\n\
         default {}\ntimeout 0\neditor no\n",
        image(name)
    )
}

/// The kernel that legionctl's `loader.conf` starts; `None` when the boot takeover is not installed.
///
/// # Errors
/// The mount table or `loader.conf` cannot be read.
pub fn default_kernel(root: &SysRoot) -> Result<Option<String>> {
    let Some((esp, _)) = esp(root)? else {
        return Ok(None);
    };
    let Some(conf) = root
        .read(Path::new(&esp).join(LOADER))?
        .filter(|c| c.starts_with(LOADER_MARK))
    else {
        return Ok(None);
    };
    Ok(conf.lines().find_map(|l| {
        Some(
            l.strip_prefix("default ")?
                .trim()
                .strip_prefix(UKI_PREFIX)?
                .strip_suffix(".efi")?
                .to_string(),
        )
    }))
}

fn line_file(name: &str) -> String {
    format!("{LINES}/{name}.cmdline")
}

fn old_file(name: &str) -> String {
    format!("{LINES}/{name}.cmdline.old")
}

/// The command line of the kernel `name`, the source of its image; `None` until the boot takeover writes it.
///
/// # Errors
/// The line file cannot be read.
pub fn saved_line(root: &SysRoot, name: &str) -> Result<Option<String>> {
    root.read(line_file(name))
}

/// Refuse a command line that cannot start the kernel `name` or that the kernel would ignore in part: no `root=`, or
/// a `<module>.<parameter>` for a module that this kernel does not have (built in or loadable, DKMS too). The kernel
/// ignores such a parameter without a word. Some built-in modules have no entry in `modules.builtin` (`rcupdate`,
/// `rcutree`, `pcie_aspm`, `module`): a module that the running kernel has in `/sys/module` is accepted; for another
/// kernel, with a warning (the return value), because the other kernel can be built in a different way.
///
/// # Errors
/// The line, or the kernel's module tree, fails.
pub fn check_line(root: &SysRoot, name: &str, line: &str) -> Result<Vec<String>> {
    let words: Vec<&str> = line.split_whitespace().collect();
    anyhow::ensure!(
        words.iter().any(|w| w.starts_with("root=")),
        "the command line of {name} has no root=: the kernel would not find the root file system"
    );
    let release = crate::kernel::release_of(root, name)?
        .with_context(|| format!("no module tree of {name} in /usr/lib/modules"))?;
    let modules = crate::kernel::modules(root, &release)?;
    let running = crate::kernel::running_package(root)?;
    let mut warnings = Vec::new();
    for word in words {
        let key = word.split('=').next().unwrap_or(word);
        let Some((module, _)) = key.split_once('.') else {
            continue;
        };
        let module_name = module.replace('-', "_");
        if USER_SPACE.contains(&module) || modules.contains(&module_name) {
            continue;
        }
        if !root.exists(format!("/sys/module/{module_name}"))? {
            bail!(
                "{word}: {name} ({release}) has no module {module}, built in or loadable; the kernel would ignore it"
            );
        }
        if running.as_deref() != Some(name) {
            warnings.push(format!(
                "{word}: the running kernel has the module {module} (/sys/module), but the files of {name} \
                 ({release}) do not list it; if {name} does not have it, {name} ignores this parameter"
            ));
        }
    }
    Ok(warnings)
}

/// The running system's kernel line, without what the boot manager adds (`initrd=`, `BOOT_IMAGE=`).
fn cmdline(root: &SysRoot) -> Result<String> {
    let line = root
        .read("/proc/cmdline")?
        .context("reading /proc/cmdline: no such file")?;
    Ok(line
        .split_whitespace()
        .filter(|w| !w.starts_with("initrd=") && !w.starts_with("BOOT_IMAGE="))
        .collect::<Vec<_>>()
        .join(" "))
}

/// The running kernel's name in `/boot` (`linux-cachyos-bore-lto`), from the `initrd=` the boot manager passed.
fn running_kernel(root: &SysRoot) -> Result<String> {
    let line = root
        .read("/proc/cmdline")?
        .context("reading /proc/cmdline: no such file")?;
    let from_line = line.split_whitespace().find_map(|w| {
        let image = w.strip_prefix("initrd=")?.rsplit(['\\', '/']).next()?;
        Some(
            image
                .strip_prefix("initramfs-")?
                .strip_suffix(".img")?
                .to_string(),
        )
    });
    match from_line {
        Some(name) => Ok(name),
        None => kernels(root)?
            .into_iter()
            .next()
            .context("no running kernel in /proc/cmdline and no kernel in /boot"),
    }
}

/// The boot tools that a plan runs, each with the Arch package that has it.
const TOOLS: [(&str, &str); 4] = [
    ("ukify", "systemd-ukify"),
    ("sbsign", "sbsigntools"),
    ("sbverify", "sbsigntools"),
    ("efibootmgr", "efibootmgr"),
];

/// `steps` when each boot tool that they run is on `PATH`; for the dry run too, so a plan never stops half done at a
/// missing tool (todo-149).
///
/// # Errors
/// A tool is missing: the error names it and its package.
pub fn tools_checked(steps: Vec<Step>) -> Result<Vec<Step>> {
    let mut missing: Vec<String> = Vec::new();
    let mut packages: Vec<&str> = Vec::new();
    for step in &steps {
        let (Step::Run(argv) | Step::Unit(argv)) = step else {
            continue;
        };
        let Some((tool, package)) = TOOLS
            .iter()
            .find(|(t, _)| argv.first().is_some_and(|a| a == t))
        else {
            continue;
        };
        let name = format!("{tool} ({package})");
        if !missing.contains(&name) && crate::policy::which(tool).is_none() {
            missing.push(name);
            if !packages.contains(package) {
                packages.push(package);
            }
        }
    }
    anyhow::ensure!(
        missing.is_empty(),
        "the plan runs {}, which is not installed: install it with pacman -S {}; nothing was changed",
        missing.join(", "),
        packages.join(" ")
    );
    Ok(steps)
}

fn text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn mkdir(root: &SysRoot, dir: impl AsRef<Path>) -> Step {
    Step::Run(vec!["mkdir".into(), "-p".into(), text(&root.path(dir))])
}

/// The saved sets with boot entries (`rules sets save --boot`, todo-85).
fn boot_sets(root: &SysRoot) -> Result<Vec<String>> {
    Ok(crate::ledger::Ledger::read(root)?
        .sets()?
        .into_iter()
        .filter(|(_, s)| s.boot)
        .map(|(name, _)| name)
        .collect())
}

/// The image `file` (in `EFI/Linux`) of the kernel `name` with `line`: made beside the old one, signed by `ukify` with the MOK key, its
/// signature checked, and only then put in place of the old one. Each saved set with boot entries is a profile of the
/// image (a multi-profile UKI): its entry starts the kernel with `line` and `legionctl.set=<set>`; profile 0, the
/// default entry, has the kernel's own line.
fn build(root: &SysRoot, esp: &str, name: &str, line: &str, file: &str) -> Result<Vec<Step>> {
    let mut ucode = None;
    for u in ["/boot/intel-ucode.img", "/boot/amd-ucode.img"] {
        if root.exists(u)? {
            ucode = Some(u);
            break;
        }
    }
    let target = text(&root.path(Path::new(esp).join("EFI/Linux").join(file)));
    let new = format!("{target}.new");
    let profiles: Vec<(String, String)> = boot_sets(root)?
        .into_iter()
        .map(|set| (format!("{target}.{set}.profile"), set))
        .collect();
    let mut steps: Vec<Step> = profiles
        .iter()
        .map(|(file, set)| {
            Step::Unit(vec![
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
        Step::Unit(argv),
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
    Ok(steps)
}

/// The folders of the images and of the command lines.
fn image_dirs(root: &SysRoot, esp: &str) -> [Step; 2] {
    [
        mkdir(root, Path::new(esp).join("EFI/Linux")),
        mkdir(root, LINES),
    ]
}

/// The command line of the kernel `name`: its own, else the running system's line, which the steps write down as
/// its own.
fn line_of(root: &SysRoot, name: &str) -> Result<(String, Vec<Step>)> {
    if let Some(line) = saved_line(root, name)? {
        return Ok((line, Vec::new()));
    }
    let line = cmdline(root)?;
    let step = Step::Create {
        path: line_file(name),
        content: format!("{line}\n"),
    };
    Ok((line, vec![step]))
}

/// Each kernel in `/boot` with its image steps, or why its line fails [`check_line`]. One kernel's failure does not
/// touch the steps of another kernel.
fn uki_steps(root: &SysRoot, esp: &str) -> Result<Vec<(String, Result<Vec<Step>>)>> {
    let names = kernels(root)?;
    anyhow::ensure!(!names.is_empty(), "no kernel with an initramfs in /boot");
    Ok(names
        .into_iter()
        .map(|name| {
            let steps = line_of(root, &name).and_then(|(line, mut steps)| {
                let warnings = check_line(root, &name, &line)?;
                steps.extend(warnings.into_iter().map(Step::Warn));
                steps.extend(build(root, esp, &name, &line, &image(&name))?);
                Ok(steps)
            });
            (name, steps)
        })
        .collect())
}

/// The kernels whose image the boot takeover cannot make again, each with the reason (a `doctor` finding): their line
/// fails [`check_line`], so `boot rebuild` (the pacman hook) leaves their old image.
///
/// # Errors
/// `/boot` or the ESP cannot be read.
pub fn unbuilt(root: &SysRoot) -> Result<Vec<String>> {
    if default_kernel(root)?.is_none() {
        return Ok(Vec::new());
    }
    Ok(kernels(root)?
        .into_iter()
        .filter_map(|name| {
            let e = line_of(root, &name)
                .and_then(|(line, _)| check_line(root, &name, &line))
                .err()?;
            Some(format!(
                "boot rebuild (the pacman hook) does not make the signed image of {name} again, its old image \
                 stays: {e:#}; correct its line with legionctl kernel cmdline set {name} \"<line>\""
            ))
        })
        .collect())
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
    tools_checked(vec![
        Step::LoaderVar {
            name: "LoaderEntryOneShot".into(),
            value: image(name),
        },
        Step::Unit(vec!["efibootmgr".into(), "--bootnext".into(), id]),
        Step::Run(vec!["systemctl".into(), "reboot".into()]),
    ])
}

/// The image of the kernel `name` with a new command line, for one start before it replaces the kernel's image.
fn trial_image(name: &str) -> String {
    format!("{UKI_PREFIX}{name}.trial.efi")
}

fn trial_file(name: &str) -> String {
    format!("{LINES}/{name}.cmdline.trial")
}

/// A systemd-boot variable from efivarfs (`LoaderEntrySelected`: the entry of this start; `LoaderEntryOneShot`: the
/// entry of the next start only): the attributes, then UTF-16 with its end mark.
fn loader_var(root: &SysRoot, name: &str) -> Option<String> {
    let bytes = std::fs::read(root.path(format!(
        "/sys/firmware/efi/efivars/{name}-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f"
    )))
    .ok()?;
    let text: Vec<u16> = bytes
        .get(4..)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|c| *c != 0)
        .collect();
    String::from_utf16(&text).ok()
}

/// Plan `kernel cmdline set`, in two calls. The first call checks `line`, makes the trial image of `name` with it
/// (beside the kernel's image, which stays) and sets the next start, only, to the trial image (`LoaderEntryOneShot`
/// and `BootNext`, as `kernel try`, without the restart). The same call after a start through the trial image puts
/// the trial image in place of the kernel's image, `line` as its command line, and the line before it in
/// `<name>.cmdline.old` for `kernel cmdline undo`. A bad line thus costs one start, never the default.
///
/// # Errors
/// No boot takeover, no image of `name`, or the line fails [`check_line`].
pub fn cmdline_plan(
    root: &SysRoot,
    state: &BootState,
    name: &str,
    line: &str,
) -> Result<Vec<Step>> {
    let (id, esp) = takeover(state)?;
    startable(state, name)?;
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let warnings = check_line(root, name, &line)?;
    let before = saved_line(root, name)?.with_context(|| {
        format!("no command line of {name} in {LINES} yet (legionctl boot rebuild writes it)")
    })?;
    let trial = trial_image(name);
    // systemd-boot removes the one-time entry at the start that uses it: no one-time entry left means that this start
    // is that start, not one before the trial image was made
    let tried = root.read(trial_file(name))?.as_deref() == Some(line.as_str())
        && state.ukis.contains(&trial)
        && loader_var(root, "LoaderEntrySelected").as_deref() == Some(trial.as_str())
        && loader_var(root, "LoaderEntryOneShot").is_none();
    let linux = Path::new(&esp).join("EFI/Linux");
    let mut steps: Vec<Step> = warnings.into_iter().map(Step::Warn).collect();
    if tried {
        steps.extend([
            Step::Run(vec![
                "mv".into(),
                text(&root.path(linux.join(&trial))),
                text(&root.path(linux.join(image(name)))),
            ]),
            mkdir(root, LINES),
            Step::Create {
                path: old_file(name),
                content: format!("{before}\n"),
            },
            Step::Create {
                path: line_file(name),
                content: format!("{line}\n"),
            },
            Step::Remove {
                path: trial_file(name),
            },
        ]);
        return tools_checked(steps);
    }
    steps.extend(build(root, &esp, name, &line, &trial)?);
    steps.extend([
        mkdir(root, LINES),
        Step::Create {
            path: trial_file(name),
            content: format!("{line}\n"),
        },
        Step::LoaderVar {
            name: "LoaderEntryOneShot".into(),
            value: trial.clone(),
        },
        Step::Unit(vec!["efibootmgr".into(), "--bootnext".into(), id]),
        Step::Info(format!(
            "the next start only uses the new line ({trial}); {name}'s image and line stay. After a good start \
             through it, give the same command again: then the new line becomes {name}'s"
        )),
    ]);
    tools_checked(steps)
}

/// Plan `kernel cmdline undo`: the line before the last change, as [`cmdline_plan`] (one start through its trial
/// image, then the same command again; a second undo goes forward again).
///
/// # Errors
/// No line was changed, or [`cmdline_plan`] fails.
pub fn undo_plan(root: &SysRoot, state: &BootState, name: &str) -> Result<Vec<Step>> {
    let before = root
        .read(old_file(name))?
        .with_context(|| format!("no earlier command line of {name} to go back to"))?;
    cmdline_plan(root, state, name, &before)
}

/// The folder where the daemon copies the packages of `kernel install` before it reads and installs them: root's
/// own (0700), so the caller cannot change a file after its check.
const STAGE: &str = "/var/lib/legionctl/kernel-install";

/// `pkgname`, `pkgbase` and `pkgver` (with `pkgrel`) from the `.PKGINFO` of the package `file`.
fn package_info(file: &Path) -> Result<(String, String, String)> {
    let out = std::process::Command::new("bsdtar")
        .arg("-xOf")
        .arg(file)
        .arg(".PKGINFO")
        .output()
        .context("running bsdtar")?;
    anyhow::ensure!(
        out.status.success(),
        "{} is not a package: {}",
        file.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let info = String::from_utf8_lossy(&out.stdout);
    let field = |key: &str| {
        info.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(" = "))
            .map(String::from)
            .with_context(|| format!("{} has no {key} in .PKGINFO", file.display()))
    };
    Ok((field("pkgname")?, field("pkgbase")?, field("pkgver")?))
}

/// Plan `kernel build`'s last step (the daemon, as root): install the built packages of the kernel `name` (the
/// kernel and its headers, `<name>-<version>-<arch>.pkg.tar.zst` in `dir`) with `pacman -U`. Only a kernel that
/// `kernel build` makes (`linux-cachyos-<name>`): the packages are copied into [`STAGE`] first, and their `.PKGINFO`
/// there must name the kernel (`pkgname` the kernel or its headers, `pkgbase` the kernel), one kernel package and one
/// version. pacman installs the copies. pacman's hooks then make its initramfs, its signed image (`boot rebuild`) and
/// its DKMS modules.
///
/// # Errors
/// `name` is not `linux-cachyos-<name>` or an installed kernel already, or the packages in `dir` fail the checks.
pub fn package_plan(root: &SysRoot, name: &str, dir: &Path) -> Result<Vec<Step>> {
    anyhow::ensure!(
        name.strip_prefix("linux-cachyos-")
            .is_some_and(|n| !n.is_empty()
                && n.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')),
        "kernel install takes only a kernel of kernel build (linux-cachyos-<name>), not {name}"
    );
    anyhow::ensure!(
        !crate::kernel::installed_packages(root)?
            .iter()
            .any(|p| p == name),
        "{name} is an installed kernel already: give the build another --name"
    );
    let stage = root.path(STAGE);
    match std::fs::remove_dir_all(&stage) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(e).with_context(|| format!("removing {}", stage.display()));
        }
        _ => {}
    }
    std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
        .create(&stage)
        .with_context(|| format!("making {}", stage.display()))?;
    let mut copies: Vec<(String, String)> = Vec::new();
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .flatten()
    {
        let Ok(file) = entry.file_name().into_string() else {
            continue;
        };
        let Some(rest) = file.strip_prefix(name).and_then(|r| r.strip_prefix('-')) else {
            continue;
        };
        // the kernel (`<name>-7.2.9-2-...`) and its headers, not the debug package
        if !(file.ends_with(".pkg.tar.zst")
            && (rest.starts_with(|c: char| c.is_ascii_digit()) || rest.starts_with("headers-")))
        {
            continue;
        }
        anyhow::ensure!(
            entry.file_type()?.is_file(),
            "{} is not a plain file",
            entry.path().display()
        );
        let copy = stage.join(&file);
        std::fs::copy(entry.path(), &copy)
            .with_context(|| format!("copying {}", entry.path().display()))?;
        let (pkgname, pkgbase, version) = package_info(&copy)?;
        anyhow::ensure!(
            pkgbase == name && (pkgname == name || pkgname == format!("{name}-headers")),
            "{file}: its .PKGINFO names {pkgname} of {pkgbase}, not {name}"
        );
        copies.push((pkgname, version));
    }
    let kernels = copies.iter().filter(|(p, _)| p == name).count();
    anyhow::ensure!(
        kernels == 1,
        "{kernels} packages of {name} in {} (one is needed): remove the old ones and run the build again",
        dir.display()
    );
    let mut versions: Vec<&str> = copies.iter().map(|(_, v)| v.as_str()).collect();
    versions.sort_unstable();
    versions.dedup();
    anyhow::ensure!(
        versions.len() == 1 && copies.len() <= 2,
        "the packages of {name} in {} have more than one version ({})",
        dir.display(),
        versions.join(", ")
    );
    let mut files: Vec<String> = std::fs::read_dir(&stage)?
        .flatten()
        .map(|e| text(&e.path()))
        .collect();
    files.sort();
    Ok(vec![
        Step::Unit(
            ["pacman", "-U", "--noconfirm"]
                .map(String::from)
                .into_iter()
                .chain(files)
                .collect(),
        ),
        Step::Run(vec!["rm".into(), "-rf".into(), text(&stage)]),
    ])
}

/// Plan `boot install`: our shim copy with systemd-boot as its signed second stage, the UKIs, `loader.conf` (the
/// running kernel first, no wait), and a new firmware entry that is not first in the order. Each step can run again:
/// the next install completes one that failed on the way, and makes the firmware entry only when it is not there.
///
/// # Errors
/// No ESP, no shim on it, no MOK key, no systemd-boot, no kernels, or a boot tool is missing.
pub fn install_plan(root: &SysRoot, state: &BootState) -> Result<Vec<Step>> {
    let running = running_kernel(root)?;
    let (esp, device) =
        esp(root)?.context("no ESP is mounted at /boot/efi, /efi or /boot (vfat)")?;
    let shim = Path::new(&esp).join("EFI/BOOT/bootx64.efi");
    let manager = Path::new(&esp).join("EFI/BOOT/mmx64.efi");
    if !root.exists(&shim)? || !root.exists(&manager)? {
        bail!(
            "no shim on the ESP ({} and mmx64.efi): legionctl adds systemd-boot only beside a shim chain",
            shim.display()
        );
    }
    for need in [KEY, CERT, SYSTEMD_BOOT] {
        anyhow::ensure!(root.exists(need)?, "{need} is missing");
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
        Step::Unit(vec![
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
    steps.extend(image_dirs(root, &esp));
    for (name, kernel) in uki_steps(root, &esp)? {
        steps.extend(kernel.with_context(|| format!("the image of {name}"))?);
    }
    steps.push(mkdir(root, Path::new(&esp).join("loader")));
    // the running kernel is the default at first
    steps.push(Step::Create {
        path: text(&Path::new(&esp).join(LOADER)),
        content: loader_conf(&running),
    });
    if state.ours.is_some() {
        return tools_checked(steps);
    }
    let partition = device.trim_start_matches("/dev/");
    let number = root
        .read(format!("/sys/class/block/{partition}/partition"))?
        .context("the ESP's partition number")?;
    // the partition's sysfs folder is inside its disk's: .../nvme1n1/nvme1n1p1
    let disk = root
        .link(format!("/sys/class/block/{partition}"))?
        .and_then(|p| {
            p.parent()?
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .context("the ESP's disk")?;
    steps.push(Step::Unit(vec![
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
    tools_checked(steps)
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
    let step = Step::Unit(vec!["efibootmgr".into(), "--bootnext".into(), id]);
    Ok(tools_checked(vec![step])?.remove(0))
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
    tools_checked(vec![Step::Unit(vec![
        "efibootmgr".into(),
        "--bootorder".into(),
        order.join(","),
    ])])
}

/// Plan `boot rebuild` (the pacman hook): each kernel's UKI again, and the UKIs of kernels that are gone removed. A
/// kernel whose line fails [`check_line`] keeps its old image and gets a warning; the other kernels are made again.
/// When the default kernel is gone, `loader.conf` starts the running kernel, else the first kernel with an image.
///
/// # Errors
/// No ESP or no kernels.
pub fn rebuild_plan(root: &SysRoot, state: &BootState) -> Result<Vec<Step>> {
    let esp = state.esp.clone().context("no ESP is mounted")?;
    let mut steps = image_dirs(root, &esp).to_vec();
    let mut with_image = Vec::new();
    for (name, kernel) in uki_steps(root, &esp)? {
        match kernel {
            Ok(kernel) => {
                steps.extend(kernel);
                with_image.push(name);
            }
            Err(e) => {
                steps.push(Step::Warn(format!(
                    "the image of {name} is not made again, its old image stays: {e:#}"
                )));
                if state.ukis.contains(&image(&name)) {
                    with_image.push(name);
                }
            }
        }
    }
    for uki in &state.ukis {
        let kernel = uki.trim_start_matches(UKI_PREFIX).trim_end_matches(".efi");
        if !state.kernels.iter().any(|k| k == kernel) {
            steps.push(Step::Remove {
                path: text(&Path::new(&esp).join("EFI/Linux").join(uki)),
            });
        }
    }
    if let Some(gone) = default_kernel(root)?.filter(|d| !state.kernels.contains(d)) {
        let running = crate::kernel::running_package(root)?.filter(|r| with_image.contains(r));
        match running.or_else(|| with_image.first().cloned()) {
            Some(next) => steps.push(Step::Create {
                path: text(&Path::new(&esp).join(LOADER)),
                content: loader_conf(&next),
            }),
            None => steps.push(Step::Warn(format!(
                "the default kernel {gone} is gone and no other kernel has an image: loader.conf stays"
            ))),
        }
    }
    tools_checked(steps)
}

/// Plan `boot remove`: our entry, our folder, our UKIs and our `loader.conf` gone; the old chain as it was.
///
/// # Errors
/// No ESP.
pub fn remove_plan(root: &SysRoot, state: &BootState) -> Result<Vec<Step>> {
    let esp = state.esp.clone().context("no ESP is mounted")?;
    let mut steps = Vec::new();
    if let Some(id) = &state.ours {
        steps.push(Step::Unit(vec![
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
        .read(&loader)?
        .is_some_and(|c| c.starts_with(LOADER_MARK))
    {
        steps.push(Step::Remove {
            path: text(&loader),
        });
    }
    tools_checked(steps)
}
