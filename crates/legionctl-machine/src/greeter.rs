//! The greeter (todo-83, decided by the user 2026-10-14): greetd starts legionctl's greeter (`legionctl-greeter` in
//! `cage`) in place of GDM. GDM stays installed and disabled: `remove` brings it back. Before login, the greeter
//! changes the Windows VM, the Windows disk and the GPUs of the chosen user through the daemon (a polkit rule of the
//! package lets greetd's user `greeter` do these actions only).

use anyhow::{Context, Result, bail};

use crate::policy::Step;
use crate::sysroot::SysRoot;
use crate::winvm;

/// greetd's configuration, and the file that was there before ours.
pub const CONFIG: &str = "/etc/greetd/config.toml";
pub const BEFORE: &str = "/etc/greetd/config.toml.before";
/// The greeter binary of the package.
pub const GREETER: &str = "/usr/lib/legionctl/legionctl-greeter";
const MARK: &str = "# legionctl greeter";
/// The alias that `systemctl enable` of a display manager makes.
const DISPLAY_MANAGER: &str = "/etc/systemd/system/display-manager.service";
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

#[must_use]
pub fn state(root: &SysRoot) -> GreeterState {
    GreeterState {
        installed: root.read(CONFIG).is_some_and(|c| c.starts_with(MARK)),
        display_manager: root
            .link(DISPLAY_MANAGER)
            .and_then(|p| Some(p.file_name()?.to_string_lossy().into_owned())),
        kept: root.exists(BEFORE),
        missing: [
            ("/usr/bin/greetd", "greetd (pacman -S greetd)"),
            ("/usr/bin/cage", "cage (pacman -S cage)"),
            (GREETER, "legionctl-greeter (the legionctl package)"),
        ]
        .into_iter()
        .filter(|(path, _)| !root.exists(path))
        .map(|(_, what)| what.to_string())
        .collect(),
    }
}

fn systemctl(verb: &str, unit: &str) -> Step {
    Step::Run(vec!["systemctl".into(), verb.into(), unit.into()])
}

/// Plan `greeter install`: greetd's configuration with our greeter (the old file kept), GDM disabled, greetd enabled.
///
/// # Errors
/// The greeter is installed already, greetd, cage or the greeter is not installed, or GDM is not the display manager.
pub fn install_plan(root: &SysRoot) -> Result<Vec<Step>> {
    let now = state(root);
    if now.installed {
        bail!("legionctl's greeter is installed already ({CONFIG})");
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
    let mut steps = Vec::new();
    if let Ok(old) = std::fs::read_to_string(root.path(CONFIG)) {
        steps.push(Step::Create {
            path: BEFORE.into(),
            content: old,
        });
    }
    steps.push(Step::Create {
        path: CONFIG.into(),
        content: format!(
            "{MARK} (legionctl greeter install; legionctl greeter remove puts {BEFORE} back)\n\
             [terminal]\nvt = 1\n\n\
             [default_session]\ncommand = \"cage -s -- {GREETER}\"\nuser = \"greeter\"\n"
        ),
    });
    steps.push(systemctl("disable", GDM));
    steps.push(systemctl("enable", GREETD));
    Ok(steps)
}

/// Plan `greeter remove`: the old configuration back, greetd disabled, GDM enabled.
///
/// # Errors
/// The greeter is not installed, or GDM is not installed.
pub fn remove_plan(root: &SysRoot) -> Result<Vec<Step>> {
    if !state(root).installed {
        bail!("legionctl's greeter is not installed: {CONFIG} is not legionctl's");
    }
    let unit = format!("/usr/lib/systemd/system/{GDM}");
    anyhow::ensure!(
        root.exists(&unit),
        "GDM is not installed ({unit}): install gdm first, it is the way back"
    );
    let mut steps = match std::fs::read_to_string(root.path(BEFORE)) {
        Ok(old) => vec![
            Step::Create {
                path: CONFIG.into(),
                content: old,
            },
            Step::Remove {
                path: BEFORE.into(),
            },
        ],
        Err(_) => vec![Step::Remove {
            path: CONFIG.into(),
        }],
    };
    steps.push(systemctl("disable", GREETD));
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
        .read("/etc/passwd")
        .unwrap_or_default()
        .lines()
        .map(|l| l.split(':').collect::<Vec<_>>())
        .find(|f| f.first() == Some(&user))
        .and_then(|f| f.get(2)?.parse().ok())
        .with_context(|| format!("no user {user}"))?;
    anyhow::ensure!((1000..65534).contains(&uid), "{user} is a system account");
    Ok(uid)
}

/// The steps of a Windows VM choice of the greeter for `user`, in that user's service manager: `vm-start`, `vm-stop`,
/// `autostart-off`, `autostart-login`, `autostart-boot` (as `legionctl windows vm autostart`).
///
/// # Errors
/// Another action; or a start without the user's autostart rule: the VM's runner asks polkit as that user, and before
/// login no one can answer.
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
            if root.read(winvm::AUTOSTART_RULE).as_deref() != Some(rule.trim()) {
                bail!(
                    "the Windows VM of {user} starts before login only with autostart on: its start asks polkit as \
                     {user}, and before login no one can answer"
                );
            }
            vec![manager, user_systemctl(&["start", "--no-block"])]
        }
        "vm-stop" => vec![user_systemctl(&["stop"])],
        "autostart-off" => vec![
            Step::Remove {
                path: winvm::AUTOSTART_RULE.into(),
            },
            manager,
            user_systemctl(&["disable"]),
        ],
        "autostart-login" | "autostart-boot" => {
            let mut steps = vec![
                Step::Create {
                    path: winvm::AUTOSTART_RULE.into(),
                    content: winvm::autostart_rule(user)?,
                },
                manager,
                user_systemctl(&["enable"]),
            ];
            if action == "autostart-boot" {
                steps.push(Step::Run(
                    ["loginctl", "enable-linger", user]
                        .map(String::from)
                        .to_vec(),
                ));
            }
            steps
        }
        other => bail!("{other} is not a Windows VM action of the greeter"),
    })
}
