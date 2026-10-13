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
         elif [ \"$1\" = is-active ]; then echo inactive; \
         elif [ \"$1\" = --failed ]; then echo 'tlp.service loaded failed failed TLP'; \
         elif [ \"$2\" = --failed ]; then echo 'app-ghostty.service loaded failed failed Ghostty'; \
         else echo disabled; fi",
    );
    // the OSD extension threw at startup
    Machine::script(
        &bin,
        "gnome-extensions",
        "printf 'legionctl-osd@legionctl\\n  Enabled: Yes\\n  State: ERROR\\n'",
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
    // the agent side follows: here ctx and the Codex setup are missing (setup.check's own tests cover the list)
    let (machine, agent) = lines.split_at(
        lines
            .iter()
            .position(|l| l.starts_with("- setup: "))
            .unwrap(),
    );
    assert!(
        agent.iter().all(|l| l.starts_with("- setup: ")),
        "{agent:?}"
    );
    assert!(
        agent.contains(&"- setup: ctx for all is not installed; run legionctl setup apply"),
        "{agent:?}"
    );
    assert_eq!(
        machine,
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
            "- unit tlp.service failed; see systemctl status tlp.service",
            "- user unit app-ghostty.service failed; see systemctl --user status app-ghostty.service",
            "- the OSD extension legionctl-osd@legionctl is ERROR; Fn+Q, Fn+R and Fn lock show no OSD (see journalctl --user -b /usr/bin/gnome-shell)",
            "- legionctl-system does not answer (fans, throttle reasons and the eBPF sensor are unknown): connecting to the system bus: Failed to connect to address `unix:path=/nonexistent/bus`: No such file or directory (os error 2): \
             No such file or directory (os error 2)",
        ]
    );
}

/// A recording holds every file legionctl reads: replaying it gives the decisions of the live machine, and an operator
/// on the recording shows the same status. A read that bypasses the sysroot makes this test fail.
#[test]
fn a_recording_replays_to_the_same_decisions_and_status() {
    let m = Machine::new("record");
    // unplugged with the AC policy still applied, one CPU left behind, and an Fn+Q choice in the ledger
    m.set("/sys/class/power_supply/ADP0/online", "0");
    m.set(
        "/sys/devices/system/cpu/cpufreq/policy3/energy_performance_preference",
        "power",
    );
    m.record(Key::Override, "performance");
    let json = |op: &Operator, args: &[&str]| -> serde_json::Value {
        let out = op.cli(&[args, &["--json"]].concat());
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let op = m.operator();
    let status = json(&op, &["status"]);
    // an agent's text must not reach a recording that is shared as a bug report
    std::fs::create_dir_all(m.dir.join("project")).unwrap();
    json(
        &op,
        &[
            "reminders",
            "add",
            "private",
            "the secret plan",
            "--project",
            m.dir.join("project").to_str().unwrap(),
        ],
    );
    let dir = m.dir.join("recording");
    let dir = dir.to_str().unwrap();
    let recorded = json(&op, &["record", "--dir", dir]);
    let replayed = json(&op, &["replay", "--dir", dir]);

    assert!(
        recorded["plan"].as_array().unwrap().iter().any(|l| l
            .as_str()
            .unwrap()
            .contains("policy*/energy_performance_preference")),
        "the plan sets the EPP of every CPU: {}",
        recorded["plan"]
    );
    for part in ["plan", "findings", "knobs"] {
        assert_eq!(
            recorded[part], replayed[part],
            "{part} differs on the recording"
        );
    }
    assert_eq!(
        recorded["events"].as_array().unwrap().last().unwrap()["kind"],
        "record",
        "the recording keeps its own request"
    );
    assert_eq!(recorded["events"], replayed["events"]);
    let events = std::fs::read_to_string(m.dir.join("recording/events.json")).unwrap();
    assert!(!events.contains("secret plan"), "{events}");
    assert!(
        json(&op, &["events"]).to_string().contains("secret plan"),
        "the operator's own log keeps it"
    );
    // the Fn+Q choice came along in the ledger
    let profile = &replayed["knobs"][0];
    assert_eq!(profile["id"], "platform_profile");
    assert_eq!(profile["desired"], "performance");

    // every other read operation runs on the recording too
    let on_recording = Operator::start(
        "recorded",
        &[
            ("PATH", m.dir.join("bin").into()),
            ("HOME", m.dir.join("home").into()),
            ("LEGIONCTL_SYSROOT", m.dir.join("recording/root").into()),
        ],
    );
    assert_eq!(json(&on_recording, &["status"]), status);

    let again = op.cli(&["record", "--dir", dir]);
    assert!(!again.status.success(), "a recording is never overwritten");
}

/// Two bench runs around a power-limit change: the scores come from the tools' real output (captured on this machine,
/// 2026-10-13), and compare shows the change and the knob that differs. GPU computation errors fail the command.
#[test]
fn bench_compare_shows_the_score_change_and_the_knob_that_caused_it() {
    let m = Machine::new("bench");
    let bin = m.dir.join("bin");
    let score = m.dir.join("score");
    std::fs::write(&score, "38017.46").unwrap();
    Machine::script(
        &bin,
        "stress-ng",
        &format!(
            "/usr/bin/sleep 1.2; read -r s < {}; \
             echo \"stress-ng: metrc: [697694] stressor       bogo ops real time  usr time  sys time   bogo ops/s     bogo ops/s\" >&2; \
             echo \"stress-ng: metrc: [697694]                           (secs)    (secs)    (secs)   (real time) (usr+sys time)\" >&2; \
             echo \"stress-ng: metrc: [697694] cpu              114043      3.00     91.33      0.08     $s        1247.63\" >&2; \
             echo 'stress-ng: info:  [697694] failed: 0' >&2",
            score.display()
        ),
    );
    Machine::script(
        &bin,
        "gpu-burn",
        "/usr/bin/sleep 1.2; printf 'Burning for 1 seconds.\\n\\r50.0%%  proc'\"'\"'d: 34 (21390 Gflop/s)   errors: 0   temps: 52 C \
         \\r100.0%%  proc'\"'\"'d: 68 (21427 Gflop/s)   errors: 3   temps: 54 C \\n\\nTested 1 GPUs:\\n\\tGPU 0: FAULTY\\n'",
    );
    let op = m.operator();
    let run = |args: &[&str]| -> serde_json::Value {
        let out = op.cli(&[&["bench", "run"], args, &["--json"]].concat());
        serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| panic!("{args:?}: {}", String::from_utf8_lossy(&out.stderr)))
    };

    let before = run(&["--scenario", "cpu", "--seconds", "1", "--label", "pl1-90"]);
    assert_eq!(before["scores"]["cpu bogo ops/s"], 38017.46, "{before}");
    assert!(
        before["summary"]["samples"].as_u64().unwrap() >= 1,
        "{before}"
    );
    assert_eq!(before["knobs"]["firmware.ppt_pl1_spl"], "90");
    assert_eq!(before["problems"], serde_json::json!([]));

    m.set(
        "/sys/class/firmware-attributes/lenovo-wmi-other-0/attributes/ppt_pl1_spl/current_value",
        "60",
    );
    std::fs::write(&score, "30413.97").unwrap();
    let after = run(&["--scenario", "cpu", "--seconds", "1", "--label", "pl1-60"]);

    let (a, b) = (before["id"].to_string(), after["id"].to_string());
    let compare = op.ok(&["bench", "compare", "--a", &a, "--b", &b]);
    assert!(
        compare
            .lines()
            .any(|l| l.starts_with("cpu bogo ops/s") && l.ends_with("-20.0%")),
        "{compare}"
    );
    assert!(
        compare.contains("firmware.ppt_pl1_spl: 90 -> 60"),
        "{compare}"
    );
    assert!(
        !compare.contains("cpu.epp:"),
        "only the knobs that differ: {compare}"
    );

    let gpu = op.cli(&["bench", "run", "--scenario", "gpu", "--seconds", "1"]);
    assert_eq!(gpu.status.code(), Some(1), "GPU errors fail the command");
    let text = String::from_utf8_lossy(&gpu.stdout);
    assert!(text.contains("gpu Gflop/s"), "{text}");
    assert!(
        text.contains("21427"),
        "the last progress line counts: {text}"
    );
    assert!(
        text.contains("problem: gpu-burn: 3 computation errors"),
        "{text}"
    );
    let list = op.ok(&["bench", "list"]);
    assert_eq!(list.lines().count(), 3, "{list}");
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
        assert_eq!(
            m.governors(),
            ["performance"],
            "the Fn+Q mode's governor stays after resume"
        );

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
    pub(super) type Asked = std::sync::Arc<std::sync::Mutex<Vec<(String, String, u32, bool)>>>;

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

    pub(super) struct Kill(Child);

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
    pub(super) async fn start(m: &Machine) -> (zbus::Connection, Asked, (Kill, Kill)) {
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

        let dbus = zbus::fdo::DBusProxy::new(&stand_ins).await.unwrap();
        let name = zbus::names::BusName::try_from("org.legionctl.System1").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !dbus.name_has_owner(name.clone()).await.unwrap() {
            assert!(
                Instant::now() < deadline,
                "the daemon did not take its bus name"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
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

/// The operator on the same private bus as the daemon: machine changes reach the event log, and telemetry gets the
/// root-only part from the daemon.
mod operator_and_daemon {
    use super::daemon_process::start;
    use super::*;
    use serde_json::Value;

    /// The operator of machine `m` on its private bus (the daemon's).
    fn on_bus(m: &Machine, tag: &'static str) -> Operator {
        let bus = format!("unix:path={}", m.dir.join("bus").display());
        Operator::start(
            tag,
            &[
                ("PATH", m.dir.join("bin").into()),
                ("HOME", m.dir.join("home").into()),
                ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
                ("DBUS_SYSTEM_BUS_ADDRESS", bus.into()),
            ],
        )
    }

    const PL1: &str =
        "/sys/class/firmware-attributes/lenovo-wmi-other-0/attributes/ppt_pl1_spl/current_value";

    #[tokio::test(flavor = "multi_thread")]
    async fn daemon_changes_reach_the_event_log_and_monitor_gets_the_daemons_msr_reading() {
        let m = Machine::new("telemetry");
        // on battery, so that Apply("ac") has changes to announce (no change, no Changed signal)
        m.set("/sys/class/power_supply/ADP0/online", "0");
        // the MSR with the thermal bits set (status bit 0, log bit 1) at IA32_PACKAGE_THERM_STATUS
        let msr = m.dir.join("root/dev/cpu/0/msr");
        std::fs::create_dir_all(msr.parent().unwrap()).unwrap();
        let mut bytes = vec![0u8; 0x1B1 + 8];
        bytes[0x1B1] = 0b11;
        std::fs::write(&msr, bytes).unwrap();
        m.set("/sys/class/hwmon/hwmon9/name", "coretemp");
        m.set("/sys/class/hwmon/hwmon9/temp1_input", "55000");
        // one CPU left behind by an earlier tool; the AC policy puts it back
        m.set(
            "/sys/devices/system/cpu/cpufreq/policy3/energy_performance_preference",
            "power",
        );
        let (stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "telemetry");
        let json = |args: &[&str]| -> Value {
            let out = op.cli(&[args, &["--json"]].concat());
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            serde_json::from_slice(&out.stdout).unwrap()
        };

        // telemetry: the throttle reason comes from the daemon's MSR read; the temperature from the recorded hwmon
        let sample = &json(&["monitor", "--interval-ms", "100"])[0];
        assert_eq!(
            sample["throttle_now"],
            serde_json::json!(["thermal"]),
            "{sample}"
        );
        assert_eq!(sample["throttle_logged"], serde_json::json!(["thermal"]));
        assert_eq!(sample["cpu_temp_c"], 55.0);

        // the first request starts the operator (socket activation) and with it the recorder; give it time to subscribe
        json(&["events"]);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        // a change through the daemon's API appears in the operator's event log, from the daemon
        let system = zbus::Proxy::new(
            &stand_ins,
            "org.legionctl.System1",
            "/org/legionctl/System1",
            "org.legionctl.System1",
        )
        .await
        .unwrap();
        let lines: Vec<String> = system.call("Apply", &("ac",)).await.unwrap();
        assert!(lines.iter().any(|l| l.contains("max-power")), "{lines:?}");
        let mut logged = Value::Null;
        for _ in 0..100 {
            logged = json(&["events"]);
            if logged
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["kind"] == "machine.apply")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let apply = logged
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == "machine.apply")
            .unwrap_or_else(|| panic!("not recorded: {logged}"));
        assert_eq!(apply["actor"], "legionctl-system");
        assert_eq!(apply["payload"]["lines"], serde_json::json!(lines));

        // the knob sums up every CPU; its history finds the per-CPU write through the policy* path
        let epp = json(&["inspect", "cpu.epp"]);
        assert_eq!(epp["knob"]["value"], "performance", "{epp}");
        assert_eq!(
            epp["knob"]["desired"], "balance_power",
            "on battery the battery policy is wanted, whatever was applied"
        );
        assert_eq!(epp["knob"]["policy"], "enforce");
        let history = epp["history"].as_array().unwrap();
        assert_eq!(
            history.last().map(|e| &e["id"]),
            Some(&apply["id"]),
            "{epp}"
        );
        // a knob the daemon did not touch has no history from that apply
        let untouched = json(&["inspect", "firmware.ppt_pl1_spl"]);
        assert!(
            untouched["history"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["id"] != apply["id"]),
            "{untouched}"
        );
    }

    /// A sweep of PL1 through the operator and the daemon: the stand-in workload scores what the firmware limit is at
    /// each step, so the recommendation shows that each value was really written; the original comes back; the
    /// guards refuse battery and values outside the firmware range.
    #[tokio::test(flavor = "multi_thread")]
    async fn tune_steps_the_firmware_limit_recommends_the_knee_and_restores_it() {
        let m = Machine::new("tune");
        Machine::script(
            &m.dir.join("bin"),
            "stress-ng",
            &format!(
                "/usr/bin/sleep 1.2; read -r pl1 < {}; \
                 case $pl1 in 60) s=30000.00;; 80) s=37400.00;; *) s=38000.00;; esac; \
                 echo \"stress-ng: metrc: [1] cpu              114043      3.00     91.33      0.08     $s        1247.63\" >&2",
                m.dir.join("root").join(&PL1[1..]).display()
            ),
        );
        let (_stand_ins, asked, _processes) = start(&m).await;
        let op = on_bus(&m, "tune");
        let tune = |values: &[&str]| {
            let args = [
                &[
                    "tune",
                    "--scenario",
                    "cpu",
                    "--seconds",
                    "1",
                    "--cooldown-s",
                    "0",
                    "ppt_pl1_spl",
                ][..],
                values,
            ]
            .concat();
            op.cli(&args)
        };

        let out = tune(&["90", "60", "80"]);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{text}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(text.contains("recommended ppt_pl1_spl = 80"), "{text}");
        assert!(text.contains("ppt_pl1_spl is back at 90"), "{text}");
        assert_eq!(m.file(PL1), "90", "the original value is written back");
        assert!(
            asked
                .lock()
                .unwrap()
                .iter()
                .any(|(_, action, _, _)| action == "org.legionctl.tune"),
            "polkit was asked"
        );
        let results = std::fs::read_dir(m.dir.join("root/var/lib/legionctl/tune"))
            .unwrap()
            .count();
        assert_eq!(results, 1, "one results file");

        let outside = tune(&["200"]);
        assert!(
            String::from_utf8_lossy(&outside.stderr)
                .contains("200 is outside the firmware range of ppt_pl1_spl (30..140)"),
            "{}",
            String::from_utf8_lossy(&outside.stderr)
        );
        m.set("/sys/class/power_supply/ADP0/online", "0");
        let battery = tune(&["60"]);
        assert!(
            String::from_utf8_lossy(&battery.stderr).contains("tuning runs on AC only"),
            "{}",
            String::from_utf8_lossy(&battery.stderr)
        );
    }

    /// Hotter than the firmware's own limit (`cpu_temp` 100 °C): the workload is killed at once, not after its 30 s, no
    /// later step runs, and the original value comes back.
    #[tokio::test(flavor = "multi_thread")]
    async fn tune_stops_and_kills_the_workload_above_the_firmware_temperature_limit() {
        let m = Machine::new("hot");
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "hot");
        m.set("/sys/class/hwmon/hwmon9/name", "coretemp");
        m.set("/sys/class/hwmon/hwmon9/temp1_input", "105000");
        Machine::script(&m.dir.join("bin"), "stress-ng", "/usr/bin/sleep 30");
        let started = std::time::Instant::now();
        let hot = op.cli(&[
            "tune",
            "--scenario",
            "cpu",
            "--seconds",
            "30",
            "--cooldown-s",
            "0",
            "ppt_pl1_spl",
            "60",
            "80",
        ]);
        let text = String::from_utf8_lossy(&hot.stdout);
        assert_eq!(hot.status.code(), Some(1), "a stopped sweep fails: {text}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the workload was not killed: {:?}",
            started.elapsed()
        );
        assert!(
            text.contains("stopped early: CPU at 105 °C, above the firmware limit of 100 °C"),
            "{text}"
        );
        assert_eq!(
            text.lines().filter(|l| l.starts_with("60 ")).count(),
            1,
            "no step after the stop: {text}"
        );
        assert!(!text.lines().any(|l| l.starts_with("80 ")), "{text}");
        assert_eq!(m.file(PL1), "90");
    }

    /// Without BPF permission (a non-root daemon) the daemon runs on, and the sensor's error reaches the sample and
    /// doctor.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_sensor_that_cannot_load_is_reported_by_monitor_and_doctor() {
        let m = Machine::new("nobpf");
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "nobpf");
        let out = op.cli(&["monitor", "--interval-ms", "100", "--json"]);
        let sample = &serde_json::from_slice::<Value>(&out.stdout).unwrap()[0];
        assert!(
            sample["runq_error"]
                .as_str()
                .is_some_and(|e| e.contains("Operation not permitted")),
            "{sample}"
        );
        assert_eq!(sample["runq_latency"], Value::Null);
        let doctor = String::from_utf8(op.cli(&["doctor"]).stdout).unwrap();
        assert!(
            doctor.contains(
                "- the eBPF run-queue sensor does not run: loading the runq object: map error"
            ),
            "{doctor}"
        );
    }

    /// As root (CI runs it with sudo): the eBPF programs pass the verifier of the running kernel, attach, and the
    /// histogram counts the wakeups of a busy machine.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs root: CI runs it with sudo"]
    async fn as_root_the_runq_sensor_measures_wakeups() {
        let m = Machine::new("runq");
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "runq");
        // wakeups: short sleeps in a few shells
        let load: Vec<_> = (0..4)
            .map(|_| {
                std::process::Command::new("/bin/sh")
                    .args(["-c", "for i in $(seq 200); do /usr/bin/sleep 0.005; done"])
                    .spawn()
                    .unwrap()
            })
            .collect();
        let out = op.cli(&["monitor", "--interval-ms", "1000", "--json"]);
        for mut child in load {
            child.wait().unwrap();
        }
        let samples: Value = serde_json::from_slice(&out.stdout).unwrap();
        let sample = &samples[0];
        assert_eq!(sample["runq_error"], Value::Null, "{sample}");
        let classes = sample["runq_latency"]
            .as_object()
            .unwrap_or_else(|| panic!("{sample}"));
        assert!(
            classes
                .values()
                .any(|l| l["count"].as_u64().unwrap() > 0 && l["p99_us"].as_u64().unwrap() >= 2),
            "{sample}"
        );
    }

    /// Restart into Windows: the entry comes from the firmware's list (captured on this machine), the dry run changes
    /// nothing, polkit is asked, `BootNext` is set before the reboot, and a failed `BootNext` stops the reboot.
    #[tokio::test(flavor = "multi_thread")]
    async fn windows_reboot_sets_boot_next_first_and_never_reboots_without_it() {
        let m = Machine::new("windows");
        let bin = m.dir.join("bin");
        let log = m.dir.join("commands.log");
        std::fs::write(
            m.dir.join("efibootmgr.txt"),
            "BootCurrent: 0003\nTimeout: 0 seconds\nBootOrder: 0003,0004,2002,2001,2003\n\
             Boot0003* Linpus lite\tHD(1,GPT,f35559dd-a341-4c88-9cef-b17064d991ae,0x800,0x200000)/\\EFI\\Boot\\grubx64.efiRC\n\
             Boot0004* Windows Boot Manager\tHD(1,GPT,e86fb80b-1faa-4368-80ca-0e698ba355c8,0x800,0x82000)/\\EFI\\Microsoft\\Boot\\bootmgfw.efi5749\n\
             Boot2001* EFI USB Device\tRC\n",
        )
        .unwrap();
        let efibootmgr = |fail: &str| {
            format!(
                "if [ $# -eq 0 ]; then while IFS= read -r l; do printf '%s\\n' \"$l\"; done < {}; exit 0; fi; \
                 echo \"efibootmgr $*\" >> {}; {fail}",
                m.dir.join("efibootmgr.txt").display(),
                log.display()
            )
        };
        Machine::script(&bin, "efibootmgr", &efibootmgr("exit 0"));
        Machine::script(
            &bin,
            "systemctl",
            &format!("echo \"systemctl $*\" >> {}", log.display()),
        );
        let (_stand_ins, asked, _processes) = start(&m).await;
        let op = on_bus(&m, "windows");
        let commands = || {
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .filter(|l| l.contains("bootnext") || l.contains("reboot"))
                .map(String::from)
                .collect::<Vec<_>>()
        };

        assert_eq!(
            op.ok(&["windows", "status"]),
            "Windows boots from firmware entry 0004 (Windows Boot Manager)\n"
        );
        let plan = op.ok(&["windows", "reboot", "--dry-run"]);
        assert_eq!(
            plan,
            "would run: efibootmgr --bootnext 0004\nwould run: systemctl reboot\n"
        );
        assert!(commands().is_empty(), "a dry run runs nothing");

        op.ok(&["windows", "reboot"]);
        assert_eq!(
            commands(),
            ["efibootmgr --bootnext 0004", "systemctl reboot"]
        );
        assert!(
            asked
                .lock()
                .unwrap()
                .iter()
                .any(|(_, a, _, _)| a == "org.legionctl.windows.reboot"),
            "polkit was asked"
        );

        std::fs::remove_file(&log).unwrap();
        Machine::script(
            &bin,
            "efibootmgr",
            &efibootmgr("echo 'Could not set BootNext' >&2; exit 1"),
        );
        let refused = op.cli(&["windows", "reboot"]);
        assert!(!refused.status.success());
        assert!(
            String::from_utf8_lossy(&refused.stderr)
                .contains("not rebooting: warning: command failed"),
            "{}",
            String::from_utf8_lossy(&refused.stderr)
        );
        assert_eq!(
            commands(),
            ["efibootmgr --bootnext 0004"],
            "no reboot without BootNext"
        );
    }
}
