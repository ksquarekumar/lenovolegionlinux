//! The greeter (todo-83, decided by the user 2026-10-14): greetd starts legionctl's greeter (`legionctl-greeter` in
//! `cage`) in place of GDM. GDM stays installed and disabled: `remove` brings it back. Before login, the greeter
//! changes the Windows VM, the Windows disk and the GPUs of the chosen user through the daemon (a polkit rule of the
//! package lets greetd's user `greeter` do these actions only).

use anyhow::{Context, Result, bail};

use crate::policy::Step;
use crate::sysroot::{SysRoot, read_file};
use crate::winvm;

/// greetd's configuration, and the file that was there before ours.
pub const CONFIG: &str = "/etc/greetd/config.toml";
pub const BEFORE: &str = "/etc/greetd/config.toml.before";
/// The greeter binary of the package.
pub const GREETER: &str = "/usr/lib/legionctl/legionctl-greeter";
/// The greeter's font configuration of the package: only the fonts it shows (links into noto-fonts).
pub const FONTS: &str = "/usr/share/legionctl/greeter/fonts.conf";
const MARK: &str = "# legionctl greeter";
/// The greeter's own folder (the last desktop of each user), its user's: `greeter` has `/` as its home.
pub const STATE: &str = "/var/lib/legionctl-greeter";
/// The alias that `systemctl enable` of a display manager makes.
const DISPLAY_MANAGER: &str = "/etc/systemd/system/display-manager.service";
/// greetd's unit in `/etc`: it wins over the unit of greetd's package (a drop-in cannot take away its
/// `After=plymouth-quit-wait.service`). The legionctl package owns the file with greetd's own unit in it, so the
/// daemon may write only this file; install writes legionctl's unit, remove puts the package's back.
pub const UNIT: &str = "/etc/systemd/system/greetd.service";
const UNIT_BEFORE: &str = include_str!("../../../packaging/greeter/greetd.service");
/// Where systemd-modules-load reads its lists: a file of the same name in `/etc` hides the package's.
const MODULES_ETC: &str = "/etc/modules-load.d";
/// The NVIDIA package's list in `/etc`, owned by the legionctl package with that list in it; install empties it.
/// shortcut: one name, nvidia-580xx-utils' (this machine's driver); a new driver package needs its name here.
pub const MODULES: &str = "/etc/modules-load.d/nvidia-580xx-utils.conf";
const MODULES_BEFORE: &str = include_str!("../../../packaging/greeter/nvidia-modules.conf");
const GDM: &str = "gdm.service";
const GREETD: &str = "greetd.service";

/// The login screen now.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GreeterState {
    /// greetd's configuration is legionctl's.
    pub installed: bool,
    /// The display manager that starts at boot (`gdm.service`).
    pub display_manager: Option<String>,
    /// The configuration before ours is kept.
    pub kept: bool,
    /// What the greeter needs and is not installed.
    pub missing: Vec<String>,
}

/// The greeter now.
///
/// # Errors
/// greetd's configuration or the display manager link cannot be read.
pub fn state(root: &SysRoot) -> Result<GreeterState> {
    let mut missing = Vec::new();
    for (path, what) in [
        ("/usr/bin/greetd", "greetd (pacman -S greetd)"),
        ("/usr/bin/cage", "cage (pacman -S cage)"),
        (GREETER, "legionctl-greeter (the legionctl package)"),
        (
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            "noto-fonts, the login screen's fonts (pacman -S noto-fonts)",
        ),
    ] {
        if !root.exists(path)? {
            missing.push(what.to_string());
        }
    }
    // greetd's package makes its user; greetd runs the greeter as it, and its folder is its
    let users = root.read("/etc/passwd")?.unwrap_or_default();
    if !users.lines().any(|l| l.starts_with("greeter:")) {
        missing.push("the system account greeter (the greetd package makes it)".into());
    }
    Ok(GreeterState {
        installed: root.read(CONFIG)?.is_some_and(|c| c.starts_with(MARK)),
        display_manager: root
            .link(DISPLAY_MANAGER)?
            .and_then(|p| Some(p.file_name()?.to_string_lossy().into_owned())),
        kept: root.exists(BEFORE)?,
        missing,
    })
}

fn systemctl(verb: &str, unit: &str) -> Step {
    Step::Run(vec!["systemctl".into(), verb.into(), unit.into()])
}

/// Plan `greeter install`: greetd's configuration with our greeter (the old file kept), GDM disabled, greetd enabled.
///
/// An install of an older version makes its configuration and folder again (r324 had no folder and spanned two
/// monitors).
///
/// # Errors
/// This version of the greeter is installed already, greetd, cage or the greeter is not installed, or GDM is not the
/// display manager.
pub fn install_plan(root: &SysRoot) -> Result<Vec<Step>> {
    let now = state(root)?;
    if now.installed {
        let update = update_plan(root)?;
        if update.is_empty() {
            bail!("legionctl's greeter is installed already ({CONFIG})");
        }
        return Ok(update);
    }
    if !now.missing.is_empty() {
        bail!(
            "the greeter cannot start, these are not installed:\n- {}",
            now.missing.join("\n- ")
        );
    }
    if now.display_manager.as_deref() != Some(GDM) {
        bail!(
            "the display manager is {}, not GDM: legionctl replaces only GDM (GDM is the way back)",
            now.display_manager.as_deref().unwrap_or("none")
        );
    }
    // the folder first, so a failure changes nothing of greetd or GDM
    let mut steps = vec![state_folder(root)];
    if let Some(old) = read_file(&root.path(CONFIG))? {
        steps.push(Step::Create {
            path: BEFORE.into(),
            content: old,
        });
    }
    steps.extend(
        files()
            .into_iter()
            .map(|(path, content)| Step::Create { path, content }),
    );
    steps.push(systemctl("disable", GDM));
    steps.push(systemctl("enable", GREETD));
    steps.extend(nvidia_held(root)?.map(Step::Info));
    Ok(steps)
}

/// Plan the package upgrade's step (`legionctl-system --greeter-update`): an older install of ours gets the
/// configuration and the greeter's folder of this version, and one line says what changed; the kept file and the
/// units stay as they are. None when legionctl's greeter is not installed or is of this version.
///
/// # Errors
/// greetd's configuration cannot be read, or the new configuration needs a package that is not installed.
pub fn update_plan(root: &SysRoot) -> Result<Vec<Step>> {
    if !root.read(CONFIG)?.is_some_and(|c| c.starts_with(MARK)) {
        return Ok(Vec::new());
    }
    let mut changed = Vec::new();
    for (path, content) in files() {
        // the root's read trims the text
        if root.read(&path)?.as_deref() != Some(content.trim()) {
            changed.push(Step::Create { path, content });
        }
    }
    if changed.is_empty() {
        return Ok(Vec::new());
    }
    let missing = state(root)?.missing;
    if !missing.is_empty() {
        bail!(
            "legionctl's greeter keeps its old configuration, these are not installed:\n- {}",
            missing.join("\n- ")
        );
    }
    let mut steps = vec![state_folder(root)];
    steps.extend(changed);
    steps.push(systemctl_reload());
    // change this line with the files of this version
    steps.push(Step::Info(
        "login screen: the boot splash stays until it shows, and the NVIDIA driver loads beside the boot, not before \
         it"
        .into(),
    ));
    steps.extend(nvidia_held(root)?.map(Step::Info));
    Ok(steps)
}

/// legionctl's files of the greeter, each with its content: greetd's configuration and unit, and the NVIDIA
/// package's module list emptied.
fn files() -> Vec<(String, String)> {
    vec![
        (CONFIG.to_string(), config()),
        (UNIT.to_string(), unit()),
        (
            MODULES.to_string(),
            format!(
                "{MARK}: the NVIDIA driver loads from udev beside the boot (nvidia-utils' 60-nvidia.rules loads \
                 nvidia-uvm), not before the login screen\n"
            ),
        ),
    ]
}

/// The `*.conf` files of the folder `dir` with their text, by name.
fn lists(root: &SysRoot, dir: &str) -> Result<Vec<(String, String)>> {
    let Ok(entries) = std::fs::read_dir(root.path(dir)) else {
        return Ok(Vec::new());
    };
    let mut lists = Vec::new();
    for entry in entries {
        let name = entry?.file_name().to_string_lossy().into_owned();
        // systemd reads `*.conf` with this case only
        if let Some(text) = std::path::Path::new(&name)
            .extension()
            .is_some_and(|e| e == "conf")
            .then(|| root.read(format!("{dir}/{name}")))
            .transpose()?
            .flatten()
        {
            lists.push((name, text));
        }
    }
    lists.sort();
    Ok(lists)
}

/// Whether the module list `text` loads an NVIDIA module.
fn loads_nvidia(text: &str) -> bool {
    text.lines().any(|l| l.trim_start().starts_with("nvidia"))
}

/// Doctor's finding while legionctl's greeter is the login screen and a list in `/etc` of another owner still loads
/// the NVIDIA driver before it: the one command that ends it.
///
/// # Errors
/// greetd's configuration or `/etc/modules-load.d` cannot be read.
pub fn findings(root: &SysRoot) -> Result<Vec<String>> {
    if !state(root)?.installed {
        return Ok(Vec::new());
    }
    Ok(nvidia_held(root)?.into_iter().collect())
}

/// The user's one step when a list in `/etc` that is not legionctl's still loads the NVIDIA driver before the login
/// screen: legionctl does not change a file of another owner.
fn nvidia_held(root: &SysRoot) -> Result<Option<String>> {
    let held: Vec<String> = lists(root, MODULES_ETC)?
        .into_iter()
        .map(|(name, text)| (format!("{MODULES_ETC}/{name}"), text))
        // the package's own list is legionctl's to empty, also before install empties it
        .filter(|(path, text)| path != MODULES && loads_nvidia(text))
        .map(|(path, _)| path)
        .collect();
    Ok((!held.is_empty()).then(|| {
        format!(
            "the NVIDIA driver still loads before the login screen: remove {} once (udev loads the driver): sudo rm {}",
            held.join(" and "),
            held.join(" ")
        )
    }))
}

/// legionctl's greetd unit (greetd's own, with GDM's hand-over of the boot splash): plymouth does not quit before
/// the login screen; it lets go of the screen (`deactivate`, the splash stays on it), and the greeter ends it after
/// its first frame (`legionctl-splash-quit.service`). Before that, `udevadm settle` waits for udev's loads, so the
/// NVIDIA driver and its monitors are there when cage starts (boot of 2026-10-17: nvidia-drm at 10.25 s, after a
/// 3.29 s systemd-modules-load that held sysinit.target); after 10 s the login screen starts without them.
fn unit() -> String {
    format!(
        "{MARK} (legionctl greeter install; legionctl greeter remove takes it away, greetd's own unit is back)\n\
         [Unit]\n\
         Description=Greeter daemon (legionctl: the boot splash stays until the login screen shows)\n\
         Conflicts=getty@tty1.service plymouth-quit.service\n\
         After=getty@tty1.service plymouth-quit.service plymouth-start.service systemd-user-sessions.service\n\
         OnFailure=plymouth-quit.service\n\
         StartLimitBurst=5\n\
         StartLimitIntervalSec=30\n\n\
         [Service]\n\
         Type=simple\n\
         ExecStartPre=-/usr/bin/udevadm settle --timeout=10\n\
         ExecStartPre=-/usr/bin/plymouth deactivate\n\
         ExecStart=/usr/bin/greetd\n\
         IgnoreSIGPIPE=no\n\
         SendSIGHUP=yes\n\
         TimeoutStopSec=30s\n\
         KeyringMode=shared\n\
         Restart=always\n\
         RestartSec=1\n\n\
         [Install]\n\
         Alias=display-manager.service\n"
    )
}

fn systemctl_reload() -> Step {
    Step::Run(vec!["systemctl".into(), "daemon-reload".into()])
}

/// greetd's configuration of this version: cage shows the greeter on one monitor (`-m last`: with two monitors, the
/// default `extend` spans one window over both, and the login sits at the seam); `systemd-cat` puts the greeter's
/// and cage's output in the journal (greetd gives a session the terminal, where nobody sees it). The greeter reads
/// only its own fonts (GPUI parses every system font before its first frame: 3.5 s at the boot of 2026-10-17), and
/// it has no `DISPLAY` (NVIDIA's Vulkan driver connects to it, so cage started Xwayland before the first frame).
fn config() -> String {
    format!(
        "{MARK} (legionctl greeter install; legionctl greeter remove puts {BEFORE} back)\n\
         [terminal]\nvt = 1\n\n\
         [default_session]\n\
         command = \"systemd-cat -t legionctl-greeter cage -s -m last -- env -u DISPLAY FONTCONFIG_FILE={FONTS} \
         {GREETER}\"\nuser = \"greeter\"\n"
    )
}

/// The greeter's folder for the user `greeter`, through a unit outside the daemon's sandbox (its `ReadWritePaths` do
/// not have /var/lib); the folder stays after remove.
fn state_folder(root: &SysRoot) -> Step {
    Step::Unit(
        [
            "install", "-d", "-m", "700", "-o", "greeter", "-g", "greeter",
        ]
        .map(String::from)
        .into_iter()
        .chain([root.path(STATE).to_string_lossy().into_owned()])
        .collect(),
    )
}

/// Plan `greeter remove`: the old configuration back, greetd disabled, GDM enabled.
///
/// # Errors
/// The greeter is not installed, or GDM is not installed.
pub fn remove_plan(root: &SysRoot) -> Result<Vec<Step>> {
    if !state(root)?.installed {
        bail!("legionctl's greeter is not installed: {CONFIG} is not legionctl's");
    }
    let unit = format!("/usr/lib/systemd/system/{GDM}");
    anyhow::ensure!(
        root.exists(&unit)?,
        "GDM is not installed ({unit}): install gdm first, it is the way back"
    );
    let mut steps = match read_file(&root.path(BEFORE))? {
        Some(old) => vec![
            Step::Create {
                path: CONFIG.into(),
                content: old,
            },
            Step::Remove {
                path: BEFORE.into(),
            },
        ],
        None => vec![Step::Remove {
            path: CONFIG.into(),
        }],
    };
    steps.push(systemctl("disable", GREETD));
    for (path, content) in [(UNIT, UNIT_BEFORE), (MODULES, MODULES_BEFORE)] {
        steps.push(Step::Create {
            path: path.into(),
            content: content.into(),
        });
    }
    steps.push(systemctl_reload());
    steps.push(systemctl("enable", GDM));
    Ok(steps)
}

/// The uid of `user` when it is a person's account (uid 1000 or more in `/etc/passwd`): the greeter acts only for
/// people, never for a system account.
///
/// # Errors
/// No such user, or a system account.
pub fn person(root: &SysRoot, user: &str) -> Result<u32> {
    let uid: u32 = root
        .read("/etc/passwd")?
        .unwrap_or_default()
        .lines()
        .map(|l| l.split(':').collect::<Vec<_>>())
        .find(|f| f.first() == Some(&user))
        .and_then(|f| f.get(2)?.parse().ok())
        .with_context(|| format!("no user {user}"))?;
    anyhow::ensure!((1000..65534).contains(&uid), "{user} is a system account");
    Ok(uid)
}

/// The steps of a Windows VM choice of the greeter for `user`, in that user's service manager: `vm-start` and
/// `vm-stop`. The greeter changes nothing that lasts (P-1): autostart is the user's choice after login (`legionctl
/// windows vm autostart`, with the user's own approval).
///
/// # Errors
/// Another action, an autostart choice; or a start without the user's autostart rule: the VM's runner asks polkit as
/// that user, and before login no one can answer.
pub fn vm_plan(root: &SysRoot, user: &str, uid: u32, action: &str) -> Result<Vec<Step>> {
    let unit = format!("{}.service", winvm::UNIT);
    // root reaches a user's service manager through -M user@; it runs once user@<uid> started
    let manager = systemctl("start", &format!("user@{uid}.service"));
    let user_systemctl = |verb: &[&str]| {
        let mut argv: Vec<String> = ["systemctl", "--user", "-M", &format!("{user}@")]
            .map(String::from)
            .to_vec();
        argv.extend(verb.iter().map(|w| (*w).to_string()));
        argv.push(unit.clone());
        Step::Run(argv)
    };
    Ok(match action {
        "vm-start" => {
            let rule = winvm::autostart_rule(user)?;
            if root.read(winvm::autostart_file(user)?)?.as_deref() != Some(rule.trim()) {
                bail!(
                    "the Windows VM of {user} starts before login only with autostart on: its start asks polkit as \
                     {user}, and before login no one can answer"
                );
            }
            vec![manager, user_systemctl(&["start", "--no-block"])]
        }
        "vm-stop" => vec![user_systemctl(&["stop"])],
        "autostart-off" | "autostart-login" | "autostart-boot" => bail!(
            "the greeter changes nothing that lasts: {user} turns the Windows VM autostart on or off after login \
             (`legionctl windows vm autostart`)"
        ),
        other => bail!("{other} is not a Windows VM action of the greeter"),
    })
}
