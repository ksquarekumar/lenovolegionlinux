//! The machine side against a recorded sysroot of this machine (2026-10-12, on AC with the AC policy applied):
//! reads and plans through the real operator, with stand-ins for powerprofilesctl, cpupower, systemctl and
//! nvidia-smi; and the daemon's event reactions, whose file and ledger writes run against the recorded tree.

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;

use common::Operator;

const FIXTURE: &str = "../../tests/fixtures/sysroot-82wq-2026-10-12";
const PROFILE: &str = "../../profiles/lenovo-82wq-legion-pro-7-16irx8h.json";
const INSTALLED_PROFILE: &str =
    "usr/share/legionctl/profiles/lenovo-82wq-legion-pro-7-16irx8h.json";

struct Machine {
    tag: &'static str,
    dir: PathBuf,
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        let kind = entry.file_type().unwrap();
        if kind.is_symlink() {
            // driver links point outside the tree (../../bus/pci/drivers/...); keep them as links
            std::os::unix::fs::symlink(std::fs::read_link(entry.path()).unwrap(), &target).unwrap();
        } else if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

impl Machine {
    fn new(tag: &'static str) -> Machine {
        let dir =
            std::env::temp_dir().join(format!("legionctl-machine-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE),
            &dir.join("root"),
        );
        let installed = dir.join("root").join(INSTALLED_PROFILE);
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(PROFILE),
            installed,
        )
        .unwrap();
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for name in ["cpupower", "systemctl", "nvidia-smi"] {
            Self::script(&bin, name, "exit 0");
        }
        Self::script(
            &bin,
            "powerprofilesctl",
            "if [ \"$1\" = get ]; then echo performance; fi",
        );
        Machine { tag, dir }
    }

    fn script(bin: &Path, name: &str, body: &str) {
        let path = bin.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    }

    fn file(&self, machine_path: &str) -> String {
        std::fs::read_to_string(self.dir.join("root").join(&machine_path[1..]))
            .map_or_else(|_| "<missing>".into(), |s| s.trim().to_string())
    }

    fn set(&self, machine_path: &str, value: &str) {
        let path = self.dir.join("root").join(&machine_path[1..]);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("{value}\n")).unwrap();
    }

    /// The operator on this machine; it runs the stand-in commands.
    fn operator(&self) -> Operator {
        Operator::start(
            self.tag,
            &[
                ("PATH", self.dir.join("bin").into()),
                ("HOME", self.dir.join("home").into()),
                ("LEGIONCTL_SYSROOT", self.dir.join("root").into()),
            ],
        )
    }

    fn legion(&self, args: &[&str]) -> Output {
        self.operator().cli(args)
    }

    fn ok(&self, args: &[&str]) -> String {
        self.operator().ok(args)
    }

    fn governors(&self) -> Vec<String> {
        let dir = self.dir.join("root/sys/devices/system/cpu/cpufreq");
        let mut g: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|p| {
                std::fs::read_to_string(p.path().join("scaling_governor"))
                    .unwrap()
                    .trim()
                    .into()
            })
            .collect();
        g.dedup();
        g
    }
}

const PROFILE_PATH: &str = "/sys/class/platform-profile/platform-profile-0/profile";
use legionctl_machine::ledger::{Key, Ledger};

impl Machine {
    fn ledger(&self) -> Ledger {
        Ledger::open(&legionctl_machine::sysroot::SysRoot::new(
            self.dir.join("root"),
        ))
        .unwrap()
    }

    /// A ledger value, or `<missing>`.
    fn recorded(&self, key: Key) -> String {
        self.ledger().get(key).unwrap_or_else(|| "<missing>".into())
    }

    fn record(&self, key: Key, value: &str) {
        self.ledger().set(key, Some(value)).unwrap();
    }
}

/// `make validate` and the pre-commit hook run this for the profiles in the repo.
#[test]
fn repo_profiles_are_valid() {
    let m = Machine::new("profiles");
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
    for profile in std::fs::read_dir(dir).unwrap().flatten() {
        let path = profile.path();
        let out = m.ok(&["validate", "--profile", path.to_str().unwrap()]);
        assert!(
            out.ends_with(&format!("validate: ok ({})\n", path.display())),
            "{out}"
        );
    }
}

#[test]
fn a_machine_already_in_ac_policy_gets_no_writes() {
    let m = Machine::new("steady");
    let out = m.ok(&["power", "apply", "--source", "auto", "--dry-run"]);
    assert!(!out.contains("would write"), "{out}");
}

#[test]
fn refuses_to_apply_on_a_machine_that_is_not_this_model() {
    let m = Machine::new("dmi");
    m.set("/sys/class/dmi/id/product_name", "82WK");
    m.set("/sys/class/power_supply/ADP0/online", "0");
    let out = m.legion(&["power", "apply", "--dry-run"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("product_name: expected '82WQ', got '82WK'")
    );
}

#[test]
fn doctor_lists_every_drift_and_conflict_in_a_broken_state() {
    let m = Machine::new("doctor");
    let bin = m.dir.join("bin");
    // unplugged, but the firmware and CPU still run the AC policy
    m.set("/sys/class/power_supply/ADP0/online", "0");
    // an old Legion daemon is running and its package is installed
    Machine::script(
        &bin,
        "systemctl",
        "if [ \"$1\" = is-active ] && [ \"$2\" = legiond.service ]; then echo active; \
         elif [ \"$1\" = is-active ]; then echo inactive; else echo disabled; fi",
    );
    Machine::script(&bin, "pacman", "[ \"$2\" = lenovolegionlinux-git ]");
    // acpi_call is not available for the bore-lto kernel, and DKMS built it only for the other one
    Machine::script(
        &bin,
        "modinfo",
        "if [ \"$5\" = acpi_call ] && [ \"$2\" = 7.2.9-2-cachyos-bore-lto ]; then exit 1; fi; echo /m/$5.ko",
    );
    Machine::script(
        &bin,
        "dkms",
        "echo 'acpi_call/1.2.2, 7.2.9-2-cachyos, x86_64: installed'",
    );
    Machine::script(
        &bin,
        "journalctl",
        "echo 'kernel: ntfs3(nvme0n1p3): volume is dirty'",
    );

    let out = m.legion(&["doctor"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "findings fail the command: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = report.lines().collect();
    assert_eq!(
        lines,
        [
            "doctor: warnings",
            "- platform_profile is 'max-power', expected 'balanced'",
            "- 32 cpufreq policies are not using governor 'powersave'",
            "- 32 cpufreq policies are not using EPP 'balance_power'",
            "- conflicting unit legiond.service: active=active, enabled=disabled",
            "- conflicting package is installed: lenovolegionlinux-git",
            "- required module acpi_call is missing for kernel 7.2.9-2-cachyos-bore-lto",
            "- /etc/modules-load.d/acpi.conf autoloads acpi_call, which does not exist for 7.2.9-2-cachyos-bore-lto (fails every boot)",
            "- DKMS module acpi_call/1.2.2 is not built for kernel 7.2.9-2-cachyos-bore-lto",
            "- NTFS volume nvme0n1p3 is marked dirty; run chkdsk in Windows (or ntfsfix -d, see docs/design/11-disks.md)",
            "- an external display runs on NVIDIA but the desktop may be drawn on Intel; try `legionctl gpu primary --choice nvidia`",
        ]
    );
}

#[test]
fn cleanup_plans_to_disable_running_conflicts_and_leaves_packages_to_the_human() {
    let m = Machine::new("cleanup");
    let bin = m.dir.join("bin");
    Machine::script(
        &bin,
        "systemctl",
        "if [ \"$1\" = is-active ] && [ \"$2\" = legiond.service ]; then echo active; \
         elif [ \"$1\" = is-active ]; then echo inactive; elif [ \"$1\" = is-enabled ]; then echo disabled; fi",
    );
    Machine::script(&bin, "pacman", "[ \"$2\" = lenovolegionlinux-git ]");

    let out = m.ok(&["cleanup", "--dry-run"]);
    let lines: Vec<&str> = out
        .lines()
        .filter(|l| !l.contains("reset-failed"))
        .collect();
    assert_eq!(
        lines,
        [
            "would run: systemctl disable --now legiond.service",
            "warning: conflicting packages are installed; remove them with: sudo pacman -Rns lenovolegionlinux-git",
        ],
        "quiet units stay; packages are the human's: {out}"
    );
}

/// The daemon's reactions to events against the recorded machine. `react` is what legionctl-system runs per event.
mod daemon_events {
    use super::*;
    use legionctl_machine::policy::{self, Machine as Planner, Step};
    use legionctl_machine::react::{Event, react};
    use legionctl_machine::sysroot::SysRoot;

    /// The daemon's reaction: every step as a line; file and ledger steps really run against the recorded tree,
    /// commands (powerprofilesctl, systemctl, nvidia-smi) only as planned lines, so no test touches this machine.
    fn react_and_write(m: &Machine, event: Event) -> Vec<String> {
        let (profile, _) = legionctl_machine::profile::load(Some(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join(PROFILE),
        ))
        .unwrap();
        let root = SysRoot::new(m.dir.join("root"));
        let ledger = m.ledger();
        let planner = Planner {
            root: &root,
            profile: &profile,
            ledger: &ledger,
            ppd_current: Some("performance".into()),
            has_command: &|_| true,
        };
        let steps = react(&planner, event).unwrap();
        let files: Vec<Step> = steps
            .iter()
            .filter(|s| !matches!(s, Step::Run(_) | Step::Try(_)))
            .cloned()
            .collect();
        policy::execute(&root, &ledger, &files);
        steps.iter().filter_map(policy::describe).collect()
    }

    #[test]
    fn unplug_fn_q_resume_and_replug_follow_the_policy_and_the_users_choice() {
        let m = Machine::new("cycle");

        // unplug: battery policy, firmware balanced, recorded as our own write
        m.set("/sys/class/power_supply/ADP0/online", "0");
        let lines = react_and_write(&m, Event::PowerSource);
        assert!(!lines.iter().any(|l| l.starts_with("warning")), "{lines:?}");
        assert_eq!(m.file(PROFILE_PATH), "balanced");
        assert_eq!(m.recorded(Key::Written), "balanced");
        assert_eq!(m.governors(), ["powersave"]);
        assert_eq!(
            m.file("/sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference"),
            "balance_power"
        );
        // intel_pstate refuses PPD's EPP change while the governor is performance: governors go first (and again after)
        let ppd = lines
            .iter()
            .position(|l| l == "would run: powerprofilesctl set balanced")
            .expect("PPD set");
        let first_governor = lines
            .iter()
            .position(|l| l.contains("scaling_governor"))
            .unwrap();
        assert!(first_governor < ppd, "{lines:?}");
        assert!(
            !lines.iter().any(|l| l.contains("nvidia")),
            "no NVIDIA services on battery: {lines:?}"
        );

        // our own write produces the same uevent as Fn+Q; following it changes nothing
        let lines = react_and_write(&m, Event::PlatformProfile);
        assert!(
            lines.iter().any(|l| l.contains("nothing to follow")),
            "{lines:?}"
        );

        // Fn+Q: the firmware switches to performance; that is the user's choice
        m.set(PROFILE_PATH, "performance");
        react_and_write(&m, Event::PlatformProfile);
        assert_eq!(m.recorded(Key::Override), "performance");
        assert_eq!(m.governors(), ["performance"]);

        // resume keeps the choice and does not write the firmware mode back
        react_and_write(&m, Event::Resume);
        assert_eq!(m.file(PROFILE_PATH), "performance");
        assert_eq!(m.recorded(Key::Override), "performance");

        // plug in: a power-source change starts from the profile again
        m.set("/sys/class/power_supply/ADP0/online", "1");
        let lines = react_and_write(&m, Event::PowerSource);
        assert_eq!(lines[0], "clearing firmware override 'performance'");
        assert_eq!(m.recorded(Key::Override), "<missing>");
        assert_eq!(m.file(PROFILE_PATH), "max-power");
        assert_eq!(m.recorded(Key::Written), "max-power");
        assert!(
            lines.contains(&"would run: nvidia-smi -i 0 -pm 1".into()),
            "{lines:?}"
        );
    }

    fn plan(m: &Machine, event: Event) -> Vec<String> {
        let (profile, _) = legionctl_machine::profile::load(Some(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join(PROFILE),
        ))
        .unwrap();
        let root = SysRoot::new(m.dir.join("root"));
        let ledger = m.ledger();
        let planner = Planner {
            root: &root,
            profile: &profile,
            ledger: &ledger,
            ppd_current: Some("performance".into()),
            has_command: &|_| true,
        };
        react(&planner, event)
            .unwrap()
            .iter()
            .filter(|s| !matches!(s, Step::Run(_) | Step::Try(_)))
            .filter_map(policy::describe)
            .collect()
    }

    #[test]
    fn tick_repairs_ac_drift_and_does_nothing_on_battery() {
        let m = Machine::new("tick");
        assert_eq!(
            plan(&m, Event::Tick),
            Vec::<String>::new(),
            "AC policy in place"
        );
        m.set(
            "/sys/devices/system/cpu/cpufreq/policy3/scaling_governor",
            "powersave",
        );
        assert_eq!(
            plan(&m, Event::Tick),
            ["would write /sys/devices/system/cpu/cpufreq/policy3/scaling_governor: performance"]
        );
        m.set("/sys/class/power_supply/ADP0/online", "0");
        assert_eq!(
            plan(&m, Event::Tick),
            Vec::<String>::new(),
            "no periodic writes on battery"
        );
    }

    #[test]
    fn fn_q_choice_survives_resume_and_ends_at_the_next_start() {
        let m = Machine::new("events");
        // our own write produces a platform-profile event too; it is not a choice
        m.record(Key::Written, "max-power");
        assert_eq!(
            plan(&m, Event::PlatformProfile),
            ["platform_profile 'max-power' is already the active policy; nothing to follow"]
        );
        // Fn+Q to performance
        m.set(PROFILE_PATH, "performance");
        m.record(Key::Override, "performance");
        let resume = plan(&m, Event::Resume);
        assert!(
            !resume.iter().any(|l| l.contains(PROFILE_PATH)),
            "resume keeps the choice: {resume:?}"
        );
        let start = plan(&m, Event::Start);
        assert_eq!(start[0], "clearing firmware override 'performance'");
        assert!(
            start.contains(&format!("would write {PROFILE_PATH}: max-power")),
            "{start:?}"
        );
    }
}

/// The `legionctl-system` process itself: on a private D-Bus with stand-ins for polkit and logind, against the recorded
/// machine. This is the root path that agents and the CLI reach through the operator.
mod daemon_process {
    use super::*;
    use futures_util::StreamExt;
    use std::collections::HashMap;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};
    use zbus::zvariant::OwnedValue;

    /// (subject kind, action, flags, whether anything else was attached)
    type Asked = std::sync::Arc<std::sync::Mutex<Vec<(String, String, u32, bool)>>>;

    /// Allows every action except the denied ones, as a polkit that asked a human would, and records each question:
    /// (subject kind, action, flags, whether anything else was attached).
    struct Polkit {
        denied: &'static [&'static str],
        asked: Asked,
    }

    #[zbus::interface(name = "org.freedesktop.PolicyKit1.Authority")]
    impl Polkit {
        fn check_authorization(
            &self,
            subject: (String, HashMap<String, OwnedValue>),
            action: &str,
            details: HashMap<String, String>,
            flags: u32,
            cancellation: &str,
        ) -> (bool, bool, HashMap<String, String>) {
            let extra = details.into_keys().next().is_some()
                || !cancellation.is_empty()
                || !subject.1.contains_key("name");
            self.asked
                .lock()
                .unwrap()
                .push((subject.0, action.to_string(), flags, extra));
            (!self.denied.contains(&action), false, HashMap::new())
        }
    }

    struct Kill(Child);

    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ok() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A private bus, the polkit and logind stand-ins on it, and the daemon on the recorded machine `m`. The
    /// processes stop when the returned guards drop.
    async fn start(m: &Machine) -> (zbus::Connection, Asked, (Kill, Kill)) {
        let bus_path = m.dir.join("bus");
        let bus = Kill(
            Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--nopidfile"])
                .arg(format!("--address=unix:path={}", bus_path.display()))
                .spawn()
                .unwrap(),
        );
        wait_for("the private bus", || bus_path.exists());
        let address = format!("unix:path={}", bus_path.display());
        let asked = Asked::default();
        let stand_ins = zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .name("org.freedesktop.PolicyKit1")
            .unwrap()
            .name("org.freedesktop.login1")
            .unwrap()
            .serve_at(
                "/org/freedesktop/PolicyKit1/Authority",
                Polkit {
                    denied: &["org.legionctl.gpu.desktop"],
                    asked: asked.clone(),
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap();

        let daemon_bin =
            Path::new(env!("CARGO_BIN_EXE_legionctl")).with_file_name("legionctl-system");
        let daemon = Kill(
            Command::new(daemon_bin)
                .env_clear()
                .env("PATH", m.dir.join("bin"))
                .env("LEGIONCTL_SYSROOT", m.dir.join("root"))
                .env("DBUS_SYSTEM_BUS_ADDRESS", &address)
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );

        (stand_ins, asked, (bus, daemon))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_daemon_applies_policy_on_start_and_on_d_bus_calls_resume_and_polkit_refusals() {
        let m = Machine::new("daemon");
        m.set("/sys/class/power_supply/ADP0/online", "0");
        let (stand_ins, asked, _processes) = start(&m).await;

        // start: the battery policy, written and recorded as the daemon's own
        wait_for("the battery policy at start", || {
            m.file(PROFILE_PATH) == "balanced"
        });
        let dbus = zbus::fdo::DBusProxy::new(&stand_ins).await.unwrap();
        let name = zbus::names::BusName::try_from("org.legionctl.System1").unwrap();
        while !dbus.name_has_owner(name.clone()).await.unwrap() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let system = zbus::Proxy::new(
            &stand_ins,
            "org.legionctl.System1",
            "/org/legionctl/System1",
            "org.legionctl.System1",
        )
        .await
        .unwrap();
        let mut changes = system.receive_signal("Changed").await.unwrap();

        // Apply("ac") with polkit's yes: the AC policy, and the session hears what changed and why
        // (the firmware's own platform-profile uevent, which a recorded machine does not send, announces the mode)
        let lines: Vec<String> = system.call("Apply", &("ac",)).await.unwrap();
        assert!(lines.iter().any(|l| l.contains("max-power")), "{lines:?}");
        assert_eq!(m.file(PROFILE_PATH), "max-power");
        // the daemon owns its name before it handles Start, so a Start signal may come first
        let announced = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let signal = changes.next().await.unwrap();
                let (cause, lines): (String, Vec<String>) = signal.body().deserialize().unwrap();
                if cause == "Apply" {
                    return lines;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(announced, lines);

        // resume (logind PrepareForSleep(false)) repairs what drifted while asleep, for the real power source:
        // still battery, so Apply("ac") lasts until the next event
        m.set(
            "/sys/devices/system/cpu/cpufreq/policy3/scaling_governor",
            "performance",
        );
        stand_ins
            .emit_signal(
                None::<()>,
                "/org/freedesktop/login1",
                "org.freedesktop.login1.Manager",
                "PrepareForSleep",
                &(false,),
            )
            .await
            .unwrap();
        wait_for("the resume repair", || {
            m.file("/sys/devices/system/cpu/cpufreq/policy3/scaling_governor") == "powersave"
        });

        // polkit's no: nothing changes, and the caller hears why
        let refused = system
            .call::<_, _, Vec<String>>("SetDesktopGpu", &("nvidia",))
            .await
            .unwrap_err();
        assert!(
            refused
                .to_string()
                .contains("not authorized for org.legionctl.gpu.desktop"),
            "{refused}"
        );
        assert_eq!(
            m.file("/etc/udev/rules.d/61-legionctl-mutter-primary.rules"),
            "<missing>"
        );
        // each method asked polkit about its own caller, allowing the dialog (flag 1), with nothing else attached
        let asked: Vec<(String, String, u32, bool)> = asked.lock().unwrap().clone();
        assert_eq!(
            asked,
            [
                (
                    "system-bus-name".into(),
                    "org.legionctl.power.apply".into(),
                    1,
                    false
                ),
                (
                    "system-bus-name".into(),
                    "org.legionctl.gpu.desktop".into(),
                    1,
                    false
                ),
            ]
        );
    }
}
