//! The machine side against a recorded sysroot of this machine (2026-10-12, on AC with the AC policy applied):
//! reads and plans through the real operator, with stand-ins for cpupower, systemctl and
//! nvidia-smi; and the daemon's event reactions, whose file and ledger writes run against the recorded tree.

mod common;
#[path = "common/desktop.rs"]
mod desktop;
#[path = "common/tokens.rs"]
mod tokens;

use std::path::{Path, PathBuf};
use std::process::Output;

use common::Operator;
use common::scratch::Scratch;
use tokens::add_session_token;

const FIXTURE: &str = "../../tests/fixtures/sysroot-82wq-2026-10-12";
/// The `legionctl_wmi` module's tree of this machine, recorded read-only after the user's opt-in (todo-79): its hwmon
/// device (hwmon9), firmware attributes, platform profile, WMI driver binds, `/proc/modules` and the opt-in file. It
/// goes over [`FIXTURE`].
const WMI_FIXTURE: &str = "../../tests/fixtures/sysfs-legionctl-wmi-82wq-2026-10-16";
/// The recorded module's hwmon device.
const WMI_HWMON: &str = "/sys/class/hwmon/hwmon9";
const PROFILE: &str = "../../profiles/lenovo-82wq-legion-pro-7-16irx8h.json";
const INSTALLED_PROFILE: &str =
    "usr/share/legionctl/profiles/lenovo-82wq-legion-pro-7-16irx8h.json";
const LAYERS: &str = "../../configsets/lenovo-82wq-legion-pro-7-16irx8h";
const INSTALLED_LAYERS: &str = "usr/share/legionctl/configsets/lenovo-82wq-legion-pro-7-16irx8h";

struct Machine {
    tag: &'static str,
    dir: Scratch,
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

impl Drop for Machine {
    /// As root, the BPF filesystem the daemon pinned into goes with the machine (and the pinned programs detach), before
    /// the machine's folder goes.
    fn drop(&mut self) {
        if std::os::unix::fs::MetadataExt::uid(&std::fs::metadata("/proc/self").unwrap()) == 0 {
            let _ = std::process::Command::new("umount")
                .arg(self.dir.join("root/sys/fs/bpf"))
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

impl Machine {
    fn new(tag: &'static str) -> Machine {
        let dir = Scratch::new(
            std::env::temp_dir().join(format!("legionctl-machine-{tag}-{}", std::process::id())),
        );
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE),
            &dir.join("root"),
        );
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join(WMI_FIXTURE),
            &dir.join("root"),
        );
        let installed = dir.join("root").join(INSTALLED_PROFILE);
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(PROFILE),
            installed,
        )
        .unwrap();
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join(LAYERS),
            &dir.join("root").join(INSTALLED_LAYERS),
        );
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for name in ["cpupower", "nvidia-smi"] {
            Self::script(&bin, name, "exit 0");
        }
        // as systemctl answers for units that do not run
        Self::script(
            &bin,
            "systemctl",
            "case $1 in is-active) echo inactive; exit 3;; is-enabled) echo disabled; exit 1;; esac",
        );
        // the kernel log since boot: what a test writes into kernel.log
        Self::script(
            &bin,
            "journalctl",
            &format!(
                "[ -e {log} ] && exec /usr/bin/cat {log}; exit 0",
                log = dir.join("kernel.log").display()
            ),
        );
        // a transient unit runs here: units.log gets its program; other options than the daemon's fail it
        Self::script(
            &bin,
            "systemd-run",
            &format!(
                "[ \"$1 $2 $3 $4\" = '--wait --pipe --collect --quiet' ] || exit 90; \
                 case $5 in --unit=legionctl-step-[0-9]*-[0-9]*) ;; *) exit 91;; esac; [ \"$6\" = -- ] || exit 92; \
                 shift 6; echo \"$1\" >> {}; exec \"$@\"",
                dir.join("units.log").display()
            ),
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
        // in one step, as the kernel changes an attribute: a daemon that reads during a plain write (truncate, then
        // write) reads an empty value and announces it
        let new = self.dir.join(machine_path[1..].replace('/', "%"));
        std::fs::write(&new, format!("{value}\n")).unwrap();
        std::fs::rename(new, path).unwrap();
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
    /// Mark the fan table calls verified in the installed profile (todo-17 does it on the real machine).
    fn verify_fan_calls(&self) {
        let path = self.dir.join("root").join(INSTALLED_PROFILE);
        let profile = std::fs::read_to_string(&path).unwrap();
        let marked = profile.replace("\"curve_verified\": false", "\"curve_verified\": true");
        assert_ne!(profile, marked, "the profile has a fan block");
        std::fs::write(path, marked).unwrap();
    }

    fn ledger(&self) -> Ledger {
        Ledger::open(&legionctl_machine::sysroot::SysRoot::new(
            self.dir.join("root"),
        ))
        .unwrap()
    }

    /// A ledger value, or `<missing>`.
    fn recorded(&self, key: Key) -> String {
        self.ledger()
            .get(key)
            .unwrap()
            .unwrap_or_else(|| "<missing>".into())
    }

    fn record(&self, key: Key, value: &str) {
        self.ledger().set(key, Some(value)).unwrap();
    }
}

/// The recorded `legionctl_wmi` hwmon device as the test's firmware: plain files, so the module's own refusals
/// (`EBUSY`, `EINVAL`, `EIO`) are not here; the VM test of the module checks them (e2e.rs,
/// `the_legionctl_wmi_module_gives_the_legion_wmi_through_sysfs`).
impl Machine {
    fn hwmon_file(&self, file: &str) -> String {
        self.file(&format!("{WMI_HWMON}/{file}"))
    }

    /// The fan table in the module's `pwm1_auto_point*_pwm` files.
    fn table(&self) -> [u8; 10] {
        std::array::from_fn(|i| {
            self.hwmon_file(&format!("pwm1_auto_point{}_pwm", i + 1))
                .parse()
                .unwrap()
        })
    }

    fn set_table(&self, levels: [u8; 10]) {
        for (i, level) in levels.iter().enumerate() {
            self.set(
                &format!("{WMI_HWMON}/pwm1_auto_point{}_pwm", i + 1),
                &level.to_string(),
            );
        }
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

/// What a plan needs and cannot read stops it with the cause (todo-142 F-27, F-28): a firmware attribute whose range
/// the firmware gives only in part (no `min_value`) is not written but skipped with a warning, while one with its full
/// range is written; without the AC adapter file the power source is unknown and no plan is made (before, a missing
/// file was battery and a missing bound was 0, so the battery policy or a value outside the range was written).
#[test]
fn a_plan_skips_a_firmware_value_without_its_range_and_stops_without_the_power_source() {
    let m = Machine::new("bounds");
    let attrs = "/sys/class/firmware-attributes/lenovo-wmi-other-0/attributes";
    m.set(
        "/etc/legionctl/configsets/40-site.jsonc",
        r#"{"set": {"firmware.ppt_pl1_spl": "120", "firmware.ppt_pl2_sppt": "150"}}"#,
    );
    std::fs::remove_file(
        m.dir
            .join("root")
            .join(&format!("{attrs}/ppt_pl1_spl/min_value")[1..]),
    )
    .unwrap();
    let out = m.ok(&["power", "apply", "--source", "auto", "--dry-run"]);
    assert!(
        out.contains("firmware attribute ppt_pl1_spl has only part of its range (min missing, max 140); skipped")
            && !out.contains(&format!("{attrs}/ppt_pl1_spl/current_value"))
            && out.contains(&format!("would write {attrs}/ppt_pl2_sppt/current_value: 150")),
        "{out}"
    );
    std::fs::remove_file(m.dir.join("root/sys/class/power_supply/ADP0/online")).unwrap();
    let out = m.legion(&["power", "apply", "--source", "auto", "--dry-run"]);
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success()
            && text.contains(
                "/sys/class/power_supply/ADP0/online is missing: the power source is unknown"
            ),
        "{text}"
    );
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

/// The Legion WMI driver state after the switch to `legionctl_wmi` (todo-79, todo-143). The recorded machine after the
/// user's opt-in (`legionctl_wmi` on all six devices) has no finding. The first boot with the DKMS module (2026-10-16,
/// recorded in `tests/fixtures/sysfs-wmi-mixed-82wq-2026-10-16`; the 887B54E3 bind of `lenovo_wmi_gamezone` is from the
/// boot log): doctor names the mixed bind, the devices that `legionctl_wmi` does not have and the missing profile. Back
/// on the upstream drivers without the opt-in: doctor names the missing module and the missing opt-in (the fan
/// controls need `legionctl_wmi`; there is no other path), and a missing profile without a Legion WMI driver is not
/// the profile finding.
#[test]
fn doctor_reports_a_mixed_legion_wmi_bind_and_a_missing_platform_profile() {
    let m = Machine::new("wmi-mixed");
    let root = m.dir.join("root");
    // every module exists for every kernel (a modinfo that cannot run is a check that cannot run, todo-142 F-24)
    Machine::script(&m.dir.join("bin"), "modinfo", "echo /m/$5.ko");
    let doctor = |m: &Machine| -> Vec<String> {
        let out = m.legion(&["doctor"]);
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .filter(|l| l.contains("wmi") || l.contains("platform-profile"))
            .map(String::from)
            .collect()
    };
    assert_eq!(doctor(&m), Vec::<String>::new());

    let drivers = root.join("sys/bus/wmi/drivers");
    std::fs::remove_dir_all(&drivers).unwrap();
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/sysfs-wmi-mixed-82wq-2026-10-16"),
        &root,
    );
    let profile = root.join("sys/class/platform-profile/platform-profile-0/profile");
    let recorded = std::fs::read_to_string(&profile).unwrap();
    std::fs::remove_file(&profile).unwrap();
    assert_eq!(
        doctor(&m),
        [
            "- the Legion WMI devices are bound to legionctl_wmi and to the upstream lenovo_wmi drivers: legionctl_wmi \
             (92549549-4BDE-4F06-AC04-CE8BF898DBAA-5, DC2A8805-3A8C-41BA-A6F7-092E0089CD3B-8); lenovo_wmi_gamezone \
             (887B54E3-DDDC-4B2C-8B88-68A26A8835D0-4); neither side has all of them",
            "- legionctl_wmi is loaded but does not have the Legion WMI devices 887B54E3-DDDC-4B2C-8B88-68A26A8835D0, \
             7A8F5407-CB67-4D6E-B547-39B3BE018154, D320289E-8FEA-41E0-86F9-911D83151B5F, \
             CE6C0974-0407-4F50-88BA-4FC3B6559AD8: the fan, power mode and firmware attribute files of these devices \
             are missing",
            "- /sys/class/platform-profile/platform-profile-0/profile is missing while legionctl_wmi, \
             legionctl_wmi_gamezone, lenovo_wmi_gamezone is loaded: legionctl cannot set the power mode",
        ]
    );

    // back on the upstream drivers, with their profile and without the opt-in
    std::fs::remove_dir_all(drivers.join("legionctl_wmi")).unwrap();
    std::fs::remove_dir_all(drivers.join("legionctl_wmi_gamezone")).unwrap();
    std::fs::remove_file(root.join("etc/modprobe.d/legionctl-wmi.conf")).unwrap();
    let modules = std::fs::read_to_string(root.join("proc/modules")).unwrap();
    std::fs::write(
        root.join("proc/modules"),
        modules
            .lines()
            .filter(|l| !l.starts_with("legionctl_wmi "))
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    std::fs::write(&profile, recorded).unwrap();
    let upstream = [
        "- required module legionctl_wmi is not loaded",
        "- /etc/modprobe.d/legionctl-wmi.conf is missing: legionctl_wmi does not load at start, and the fan controls \
         fail without it",
    ];
    assert_eq!(doctor(&m), upstream);

    // no Legion WMI driver: a missing profile is not this finding
    std::fs::remove_dir_all(drivers.join("lenovo_wmi_gamezone")).unwrap();
    std::fs::remove_file(&profile).unwrap();
    assert_eq!(doctor(&m), upstream);
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
         elif [ \"$1\" = --failed ]; then printf '%s\\n' 'mnt-windows\\x2ddata.mount loaded failed failed W'; \
         elif [ \"$2\" = --failed ]; then echo 'app-ghostty.service loaded failed failed Ghostty'; \
         else echo disabled; fi",
    );
    // the OSD extension threw at startup
    Machine::script(
        &bin,
        "gnome-extensions",
        "printf 'legionctl-osd@legionctl\\n  Enabled: Yes\\n  State: ERROR\\n'",
    );
    Machine::script(
        &bin,
        "pacman",
        "[ \"$2\" = lenovolegionlinux-git ] || { echo \"error: package '$2' was not found\" >&2; exit 1; }",
    );
    // legionctl_wmi and the autoloaded acpi_call are not available for the bore-lto kernel; DKMS built legionctl_wmi
    // only for the other one
    Machine::script(
        &bin,
        "modinfo",
        "if [ \"$5\" = legionctl_wmi -o \"$5\" = acpi_call ] && [ \"$2\" = 7.2.9-2-cachyos-bore-lto ]; then exit 1; fi; echo /m/$5.ko",
    );
    Machine::script(
        &bin,
        "dkms",
        "echo 'legionctl-wmi/0.1.0, 7.2.9-2-cachyos, x86_64: installed'",
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
        agent.contains(
            &"- setup: ctx for all is not installed (fix: legionctl setup apply --install-ctx)"
        ),
        "{agent:?}"
    );
    // one check of the OSD extension, one answer: setup check says what doctor says (it said "current" before)
    let osd = "OSD extension for gnome-shell is broken in this GNOME Shell: Fn+Q, Fn+R and Fn lock show no OSD";
    assert!(
        agent.contains(&format!("- setup: {osd}").as_str()),
        "{agent:?}"
    );
    let check = String::from_utf8(m.legion(&["setup", "check"]).stdout).unwrap();
    assert!(
        check
            .lines()
            .any(|l| l.contains("OSD extension") && l.contains("broken in this GNOME Shell")),
        "{check}"
    );
    assert_eq!(
        machine,
        [
            "doctor: warnings",
            "- platform_profile is 'max-power', expected 'balanced'",
            "- 32 cpufreq policies are not using governor 'powersave'",
            "- 32 cpufreq policies are not using EPP 'balance_power'",
            "- legiond.service fights legionctl (active, disabled): run legionctl cleanup",
            "- conflicting package is installed: lenovolegionlinux-git",
            "- required module legionctl_wmi is missing for kernel 7.2.9-2-cachyos-bore-lto",
            "- /etc/modules-load.d/acpi.conf autoloads acpi_call, which does not exist for 7.2.9-2-cachyos-bore-lto (fails every boot)",
            "- DKMS module legionctl-wmi/0.1.0 is not built for kernel 7.2.9-2-cachyos-bore-lto",
            "- NTFS volume nvme0n1p3 is marked dirty: Windows did not shut down fully. Start Windows and shut it down with Fast Startup off",
            "- an external display runs on NVIDIA but the desktop may be drawn on Intel (fix: legionctl gpu primary nvidia)",
            "- the system unit mnt-windows-data.mount failed",
            "- the unit app-ghostty.service of your login failed",
            "- legionctl-system does not answer: connecting to the system bus: Failed to connect to address `unix:path=/nonexistent/bus`: No such file or directory (os error 2): \
             No such file or directory (os error 2) (fans, throttle reasons and the eBPF sensor are unknown)",
            "- the system service switcheroo-control does not run: GNOME has no \"Launch using Discrete Graphics Card\"",
            "- OpenCL reaches the Intel GPU only through Mesa's rusticl, which needs RUSTICL_ENABLE=iris from the next login (or the package intel-compute-runtime) (fix: legionctl display fix opencl-iris)",
        ]
    );
}

/// U-14, user decision 2026-10-17: the package turns legionctl-system on, and the user can turn it off. Off (not on
/// the system bus), each answer that needs it says so with the one line that turns it on: about's daemon row and a
/// machine change. Doctor says it once, as setup's legionctl-system row (its action is setup turn-on, the app's Turn
/// on), not again as the daemon's sample failure.
#[test]
fn an_off_daemon_says_so_with_the_line_that_turns_it_on() {
    let m = Machine::new("daemon-off");
    let path = m.dir.join("bus");
    let _bus = crate::common::process::Kill::spawn(
        std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--nopidfile"])
            .arg(format!("--address=unix:path={}", path.display())),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !path.exists() {
        assert!(std::time::Instant::now() < deadline, "the private bus");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let op = Operator::start(
        "daemon-off",
        &[
            ("PATH", m.dir.join("bin").into()),
            ("HOME", m.dir.join("home").into()),
            ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
            (
                "DBUS_SYSTEM_BUS_ADDRESS",
                format!("unix:path={}", path.display()).into(),
            ),
        ],
    );
    let off = "legionctl-system is off; turn it on with `sudo systemctl enable --now legionctl-system.service`";
    // the MSR is root's: status reads it through the daemon only, never itself (here the file is readable)
    let msr = m.dir.join("root/dev/cpu/0/msr");
    std::fs::create_dir_all(msr.parent().unwrap()).unwrap();
    let mut bytes = vec![0u8; 0x1B1 + 8];
    bytes[0x1B1] = 0b1000;
    std::fs::write(&msr, bytes).unwrap();
    let status = op.ok(&["status"]);
    assert!(
        status.contains("throttle logged: unknown (legionctl-system is off)"),
        "{status}"
    );
    let about = op.ok(&["about"]);
    assert!(
        about
            .lines()
            .any(|l| l.contains("daemon") && l.contains(off)),
        "{about}"
    );
    let change = op.cli(&["power", "apply", "--source", "auto"]);
    let err = String::from_utf8_lossy(&change.stderr);
    assert!(!change.status.success() && err.contains(off), "{err}");
    let doctor = String::from_utf8(op.cli(&["doctor"]).stdout).unwrap();
    assert!(
        doctor
            .contains("- setup: legionctl-system for machine is off (fix: legionctl setup turn-on legionctl-system)")
            && !doctor.contains("legionctl-system is off")
            && !doctor.contains("CPU throttling logged"),
        "{doctor}"
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
    let added = op.cli_in(
        &m.dir.join("project"),
        &["reminders", "add", "private", "the secret plan"],
    );
    assert!(added.status.success(), "{added:?}");
    let dir = m.dir.join("recording");
    let dir = dir.to_str().unwrap();
    let recorded = json(&op, &["record", dir]);
    let replayed = json(&op, &["replay", dir]);

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
    let log = json(&op, &["events"]);
    assert_eq!(
        log["items"].as_array().unwrap().last().unwrap()["kind"],
        "record",
        "the operator's audit keeps the request after it ran"
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

    let again = op.cli(&["record", dir]);
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

    let before = run(&["cpu", "--seconds", "1", "--label", "pl1-90"]);
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
    let after = run(&["cpu", "--seconds", "1", "--label", "pl1-60"]);

    let (a, b) = (before["id"].to_string(), after["id"].to_string());
    let compare = op.ok(&["bench", "compare", &a, &b]);
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

    let gpu = op.cli(&["bench", "run", "gpu", "--seconds", "1"]);
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

/// The recorded machine (no /dev/kvm in the recording) with `available_gib` of 32 GiB memory available, a stand-in
/// Docker Engine API (its POSTs logged to `docker.log`) and stand-in systemctl and virsh (VM `win11` shut off, `lab`
/// running with 2 GiB resident and 2 vCPUs; virsh logs its other calls to `virsh.log`); P-cores 0-15, E-cores 16-31.
fn virt_machine(
    tag: &'static str,
    available_gib: u64,
) -> (Machine, daemon_process::Kill, Operator) {
    let m = Machine::new(tag);
    m.set("/sys/devices/cpu_core/cpus", "0-15\n");
    m.set("/sys/devices/cpu_atom/cpus", "16-31\n");
    m.set("/sys/devices/system/cpu/online", "0-31\n");
    m.set(
        "/proc/meminfo",
        &format!(
            "MemTotal:       {} kB\nMemFree:        1024 kB\nMemAvailable:   {} kB\n",
            32 << 20,
            available_gib << 20
        ),
    );
    let bin = m.dir.join("bin");
    m.set("/run/systemd/system/.present", "");
    Machine::script(
        &bin,
        "systemctl",
        "case \"$2\" in libvirtd.service|virtqemud.socket) echo active;; *) echo inactive;; esac",
    );
    Machine::script(
        &bin,
        "virsh",
        &format!(
            "if [ \"$3\" = --readonly ]; then case \"$4\" in \
             list) printf ' Id   Name    State\\n---------------------\\n -    win11   shut off\\n 3    lab     running\\n';; \
             domstats) [ \"$5 $6\" = '--balloon --list-running' ] || exit 1; \
             printf \"Domain: 'lab'\\n  balloon.current=4194304\\n  balloon.rss=2097152\\n\\n\";; esac; \
             else shift 2; echo \"$*\" >> {}; if [ \"$1\" = vcpucount ]; then echo 2; fi; fi",
            m.dir.join("virsh.log").display()
        ),
    );
    let socket = m.dir.join("docker.sock");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-docker.py");
    let docker = daemon_process::Kill::spawn(
        std::process::Command::new("python3")
            .arg(fixture)
            .arg(&socket)
            .arg(m.dir.join("docker.log")),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !socket.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the stand-in Docker did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let op = Operator::start(
        tag,
        &[
            ("PATH", bin.into()),
            ("HOME", m.dir.join("home").into()),
            ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
            ("DOCKER_HOST", format!("unix://{}", socket.display()).into()),
        ],
    );
    (m, docker, op)
}

/// `virt status` says why a value is unknown instead of a stand-in value (todo-142 F-81, F-84, user decision Q4): a
/// running VM without `balloon.rss` has no memory, with the reason (before, the balloon size, a target); virsh that
/// fails is a finding with its message (before, no VMs); a `DOCKER_HOST` that is not unix:// is Docker down with the
/// reason (before, the default socket, maybe another daemon).
#[test]
fn virt_status_says_why_a_vm_memory_libvirt_or_docker_is_unknown() {
    let (m, _docker, op) = virt_machine("virtwhy", 20);
    let bin = m.dir.join("bin");
    let virsh = |domstats: &str, list: &str| {
        Machine::script(
            &bin,
            "virsh",
            &format!(
                "case \"$4\" in list) {list};; \
                 domstats) printf \"Domain: 'lab'\\n  {domstats}\\n\\n\";; esac"
            ),
        );
    };
    virsh(
        "balloon.current=4194304",
        "printf ' Id   Name    State\\n---------------------\\n 3    lab     running\\n'",
    );
    let status: serde_json::Value =
        serde_json::from_str(&op.ok(&["virt", "status", "--json"])).unwrap();
    let lab = status["instances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == "lab")
        .unwrap_or_else(|| panic!("no lab in {status}"));
    assert!(lab["memory_bytes"].is_null(), "{lab}");
    assert!(
        lab["memory_unknown"]
            .as_str()
            .is_some_and(|w| w.starts_with("libvirt gives no balloon.rss")),
        "{lab}"
    );

    virsh(
        "",
        "echo 'error: failed to connect to the hypervisor' >&2; exit 1",
    );
    // a finding makes the exit status 1, as doctor's
    let status = String::from_utf8_lossy(&op.cli(&["virt", "status"]).stdout).to_string();
    assert!(
        status.contains(
            "- libvirt's VMs cannot be listed: virsh list --all: error: failed to connect to the hypervisor"
        ),
        "{status}"
    );

    let tcp = Operator::start(
        "virtwhy-tcp",
        &[
            ("PATH", bin.into()),
            ("HOME", m.dir.join("home").into()),
            ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
            ("DOCKER_HOST", "tcp://127.0.0.1:2375".into()),
        ],
    );
    let status: serde_json::Value =
        serde_json::from_str(&tcp.ok(&["virt", "status", "--json"])).unwrap();
    let docker = status["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["provider"] == "Docker")
        .unwrap_or_else(|| panic!("no Docker in {status}"));
    assert_eq!(docker["up"], false);
    assert_eq!(
        docker["detail"],
        "DOCKER_HOST tcp://127.0.0.1:2375: only unix:// is supported"
    );
}

/// `virt status` on the recorded machine (no /dev/kvm in the recording) with a stand-in Docker Engine API and stand-in
/// systemctl and virsh: each capability up or down, `WinBoat` recognized by its image, and the findings (no KVM, the
/// cgroupfs driver, both libvirt daemons).
#[test]
fn virt_status_reads_docker_libvirt_and_the_kernel() {
    let (_m, _docker, op) = virt_machine("virt", 20);
    let out = op.cli(&["virt", "status", "--json"]);
    let status: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let caps: Vec<(String, bool)> = status["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["name"].as_str().unwrap().into(),
                c["up"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        caps,
        [
            ("Hardware virtualization".into(), false),
            ("IOMMU".into(), false),
            ("Device passthrough".into(), false),
            ("Foreign binaries".into(), false),
            ("Containers".into(), true),
            ("GPU compute in containers".into(), false),
            ("VMs".into(), true),
        ]
    );
    assert_eq!(
        status["instances"],
        serde_json::json!([
            {"kind": "container", "name": "WinBoat", "image": "ghcr.io/dockur/windows:6.05", "state": "running",
             "consumer": "WinBoat", "memory_bytes": 16_u64 << 30, "memory_unknown": null},
            {"kind": "container", "name": "db", "image": "postgres:17", "state": "exited",
             "consumer": null, "memory_bytes": null, "memory_unknown": null},
            {"kind": "vm", "name": "win11", "image": "", "state": "shut off", "consumer": null, "memory_bytes": null,
             "memory_unknown": null},
            {"kind": "vm", "name": "lab", "image": "", "state": "running", "consumer": null, "memory_bytes": 2_u64 << 30,
             "memory_unknown": null}
        ])
    );
    assert_eq!(
        status["findings"],
        serde_json::json!([
            "no KVM: VMs run without hardware virtualization (slow)",
            "Docker uses the cgroupfs cgroup driver on a systemd host; the systemd driver is the supported setup",
            "libvirtd and the modular virtqemud both run; use one of them"
        ])
    );
}

/// `resources list` on the recorded machine with the stand-in Docker (todo-127, A1 and A2): a file system without a
/// device that fstab names and the mount table has (tmpfs on /tmp) is mounted, an automount point whose share is not
/// mounted is not; a running container offers pause and stop and an exited one start, because its state is Docker's
/// state word and not its text for people ("Up 8 hours").
#[test]
fn resources_show_a_mounted_tmpfs_and_give_a_running_container_no_start() {
    let (m, _docker, op) = virt_machine("resstate", 20);
    m.set(
        "/proc/1/mounts",
        "/dev/nvme1n1p2 / ext4 rw 0 0\ntmpfs /tmp tmpfs rw 0 0\nsystemd-1 /mnt/share autofs rw 0 0\n",
    );
    m.set(
        "/etc/fstab",
        "tmpfs /tmp tmpfs defaults 0 0\n//nas/git /mnt/share cifs noauto,x-systemd.automount 0 0\n",
    );
    let list: serde_json::Value =
        serde_json::from_str(&op.ok(&["resources", "list", "--json"])).unwrap();
    let shown = |id: &str| {
        let r = list["resources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("no {id} in {list}"));
        let actions: Vec<&str> = r["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["name"].as_str().unwrap())
            .collect();
        (r["state"].as_str().unwrap().to_string(), actions)
    };
    assert_eq!(shown("mount:/tmp"), ("mounted".into(), vec![]));
    assert_eq!(shown("mount:/mnt/share"), ("not mounted".into(), vec![]));
    assert_eq!(
        shown("virt:WinBoat"),
        ("running".into(), vec!["pause", "stop"])
    );
    assert_eq!(shown("virt:db"), ("exited".into(), vec!["start"]));
}

/// Tune: the plan alone with dry run; then a container gets its CPU set and memory limit through the Engine API, a
/// VM its vCPUs pinned one by one and its balloon set through virsh; a size that is not one is refused.
#[test]
fn virt_tune_plans_then_pins_and_limits_a_container_and_a_vm() {
    let (m, _docker, op) = virt_machine("virttune", 20);
    assert_eq!(
        op.ok(&[
            "virt",
            "tune",
            "WinBoat",
            "--cores",
            "e",
            "--memory",
            "8g",
            "--dry-run"
        ]),
        "container WinBoat: CPUs 16-31\ncontainer WinBoat: memory limit 8.0 GiB\ndry run: nothing changed\n"
    );
    assert!(
        !m.dir.join("docker.log").exists(),
        "a dry run changes nothing"
    );
    op.ok(&["virt", "tune", "WinBoat", "--cores", "e", "--memory", "8g"]);
    assert_eq!(
        std::fs::read_to_string(m.dir.join("docker.log")).unwrap(),
        "update WinBoat {\"CpusetCpus\":\"16-31\",\"Memory\":8589934592,\"MemorySwap\":8589934592}\n"
    );
    assert_eq!(
        op.ok(&["virt", "tune", "lab", "--cores", "p", "--memory", "2048m"]),
        "vm lab: 2 vCPUs on CPUs 0-15\nvm lab: memory 2.0 GiB\napplied\n"
    );
    assert_eq!(
        std::fs::read_to_string(m.dir.join("virsh.log")).unwrap(),
        "vcpucount lab --live --active\nvcpupin lab --vcpu 0 --cpulist 0-15 --live\nvcpupin lab --vcpu 1 --cpulist 0-15 --live\nsetmem lab 2097152 --live\n"
    );
    let refused = op.cli(&["virt", "tune", "lab", "--memory", "lots"]);
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("lots is not a size"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
}

/// Create: the plan alone with dry run; then the container through the Engine API (local image, CPU set, memory
/// limit; not started); a taken name and a GPU without the NVIDIA runtime are refused before anything is created.
#[test]
fn virt_create_makes_a_container_with_cores_and_memory_and_refuses_what_cannot_work() {
    let (m, _docker, op) = virt_machine("virtcreate", 20);
    let create = [
        "virt",
        "create",
        "box",
        "busybox:1",
        "--cores",
        "p",
        "--memory",
        "1g",
    ];
    assert_eq!(
        op.ok(&[&create[..], &["--dry-run"]].concat()),
        "container box: image busybox:1\ncontainer box: CPUs 0-15\ncontainer box: memory limit 1.0 GiB\n\
         dry run: nothing created\n"
    );
    assert!(!m.dir.join("docker.log").exists());
    op.ok(&create);
    let log = std::fs::read_to_string(m.dir.join("docker.log")).unwrap();
    let body: serde_json::Value =
        serde_json::from_str(log.strip_prefix("create box ").unwrap()).unwrap();
    assert_eq!(
        body,
        serde_json::json!({"Image": "busybox:1", "HostConfig": {"CpusetCpus": "0-15", "Memory": 1_073_741_824, "MemorySwap": 1_073_741_824}})
    );
    for (args, why) in [
        (
            &["virt", "create", "WinBoat", "x"][..],
            "a container named WinBoat exists",
        ),
        (
            &["virt", "create", "gpu-box", "x", "--gpu"][..],
            "Docker has no nvidia runtime",
        ),
    ] {
        let refused = op.cli(args);
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains(why),
            "{}",
            String::from_utf8_lossy(&refused.stderr)
        );
    }
    assert_eq!(
        std::fs::read_to_string(m.dir.join("docker.log"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

/// Low host memory (4 of 32 GiB available, below the 15 % `load.busy` limit): the finding names what the guests
/// hold, largest first, and how to free it; at 20 GiB available there is no such finding (the test above).
#[test]
fn virt_status_names_what_guests_hold_when_memory_is_low() {
    let (_m, _docker, op) = virt_machine("virtlow", 4);
    let status: serde_json::Value =
        serde_json::from_slice(&op.cli(&["virt", "status", "--json"]).stdout).unwrap();
    assert_eq!(
        status["findings"].as_array().unwrap().last().unwrap(),
        "memory is low (4.0 GiB of 32.0 GiB available); guests hold 18.0 GiB: WinBoat 16.0 GiB, lab 2.0 GiB. Pause \
         or stop one: legionctl virt act pause <name>"
    );
}

/// No test reaches the user's own containers: an operator that no test gives a Docker sees none (on this machine the
/// real Docker runs `WinBoat`, which a test once started by mistake through a resource action).
#[test]
fn an_operator_without_a_docker_of_its_own_reaches_no_docker() {
    let m = Machine::new("nodocker");
    let status: serde_json::Value =
        serde_json::from_slice(&m.legion(&["virt", "status", "--json"]).stdout).unwrap();
    let containers = status["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Containers")
        .unwrap();
    assert_eq!(containers["up"], false, "{containers}");
    assert!(
        containers.to_string().contains("/nonexistent/docker.sock"),
        "{containers}"
    );
    assert!(
        !status["instances"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == "container"),
        "{status}"
    );
}

/// `cargo test -p legionctl` builds only `legionctl`: a daemon built before a source it depends on changed is refused,
/// so no test runs it. Checked with cargo's own dep-info of the daemon beside a stand-in binary.
#[test]
fn a_daemon_older_than_a_source_it_was_built_from_is_refused() {
    let real = crate::common::sibling("legionctl-system");
    let deps = std::fs::read_to_string(real.with_extension("d")).unwrap();
    let changed = deps
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .skip(1)
        .map(|s| std::fs::metadata(s).unwrap().modified().unwrap())
        .max()
        .unwrap();
    let dir =
        Scratch::new(std::env::temp_dir().join(format!("legionctl-stale-{}", std::process::id())));
    let bin = dir.join("legionctl-system");
    std::fs::write(bin.with_extension("d"), &deps).unwrap();
    let file = std::fs::File::create(&bin).unwrap();

    // built one second before the newest source changed: refused, with a source
    file.set_modified(changed - std::time::Duration::from_secs(1))
        .unwrap();
    let refused = std::panic::catch_unwind(|| crate::common::fresh(&bin)).unwrap_err();
    let refused = refused.downcast_ref::<String>().unwrap();
    assert!(
        refused.starts_with(&format!(
            "{} is older than this build: run cargo build --profile test-fast first (",
            bin.display()
        )),
        "{refused}"
    );
    // built after it: accepted
    file.set_modified(changed).unwrap();
    assert_eq!(crate::common::fresh(&bin), bin);
}

/// Safe mode: after repeated failures systemd runs `legionctl-system --safe`, which hands the machine to the
/// firmware's balanced mode and leaves the reason, which `doctor` shows first. A stop that timed out (the upgrade of
/// 2026-10-16, todo-125) is no repeated failure: systemd's result for it changes nothing. A run without systemd's
/// result is refused; a run that cannot set the mode says why in the marker and fails.
#[test]
fn safe_mode_hands_the_machine_to_the_firmware_and_doctor_says_why() {
    let m = Machine::new("safe");
    assert_eq!(m.file(PROFILE_PATH), "max-power");
    let daemon = crate::common::sibling("legionctl-system");
    let safe = |result: Option<&str>| {
        let mut cmd = std::process::Command::new(&daemon);
        cmd.arg("--safe")
            .env_clear()
            .env("LEGIONCTL_SYSROOT", m.dir.join("root"));
        if let Some(result) = result {
            cmd.env("MONITOR_SERVICE_RESULT", result);
        }
        cmd.output().unwrap()
    };
    // a run by hand (no result from systemd) is refused and changes nothing (todo-142 F-12)
    let out = safe(None);
    assert!(
        !out.status.success()
            && String::from_utf8_lossy(&out.stderr).contains(
                "MONITOR_SERVICE_RESULT is not set: --safe runs only from legionctl-safe.service"
            ),
        "{out:?}"
    );
    assert_eq!(m.file(PROFILE_PATH), "max-power");
    assert_eq!(m.file("/run/legionctl/safe-mode"), "<missing>");
    // no firmware modes to read: the marker says why nothing was set, and the run fails (F-13)
    let choices = m
        .dir
        .join("root/sys/class/platform-profile/platform-profile-0/choices");
    let kept = std::fs::read(&choices).unwrap();
    std::fs::remove_file(&choices).unwrap();
    let out = safe(Some("start-limit-hit"));
    assert!(!out.status.success(), "{out:?}");
    assert!(
        m.file("/run/legionctl/safe-mode").contains(
            "the firmware mode did not change: /sys/class/platform-profile/platform-profile-0/choices is missing."
        ),
        "{}",
        m.file("/run/legionctl/safe-mode")
    );
    assert_eq!(m.file(PROFILE_PATH), "max-power");
    std::fs::write(&choices, kept).unwrap();
    let out = safe(Some("timeout"));
    assert!(out.status.success());
    assert_eq!(m.file(PROFILE_PATH), "max-power");
    let out = safe(Some("start-limit-hit"));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(m.file(PROFILE_PATH), "balanced");
    let doctor = String::from_utf8(m.legion(&["doctor"]).stdout).unwrap();
    assert_eq!(
        doctor.lines().nth(1),
        Some(
            "- safe mode: legionctl's machine service stopped after repeated failures; the firmware mode is now \
             balanced. Restart the computer to start it again"
        ),
        "{doctor}"
    );
}

/// Pause, stop and start by name: a container through the Engine API, a VM through virsh (suspend, shutdown,
/// start); a name that is neither is refused.
#[test]
fn virt_act_pauses_a_container_and_stops_a_vm_by_name() {
    let (m, _docker, op) = virt_machine("virtact", 20);
    assert_eq!(
        op.ok(&["virt", "act", "pause", "WinBoat"]),
        "container WinBoat: pause\n"
    );
    assert_eq!(op.ok(&["virt", "act", "stop", "win11"]), "vm win11: stop\n");
    // start shares its schema entry with unpause; the CLI must still offer it
    assert_eq!(
        op.ok(&["virt", "act", "start", "win11"]),
        "vm win11: start\n"
    );
    let refused = op.cli(&["virt", "act", "pause", "nothing"]);
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("no container or VM named nothing"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(m.dir.join("docker.log")).unwrap(),
        "pause WinBoat\n"
    );
    assert_eq!(
        std::fs::read_to_string(m.dir.join("virsh.log")).unwrap(),
        "shutdown win11\nstart win11\n"
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
    Machine::script(
        &bin,
        "pacman",
        "[ \"$2\" = lenovolegionlinux-git ] || { echo \"error: package '$2' was not found\" >&2; exit 1; }",
    );

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
/// A registry hive file (regf) as Windows writes one: the base block, one bin of cells, keys (`nk`) with `lh` subkey
/// lists, and DWORD values (`vk`) whose data is inline.
struct HiveFile {
    bin: Vec<u8>,
}

impl HiveFile {
    fn new() -> HiveFile {
        let mut bin = b"hbin".to_vec();
        bin.resize(32, 0);
        HiveFile { bin }
    }

    /// A cell holding `data`; its offset from the first bin.
    fn cell(&mut self, data: &[u8]) -> u32 {
        let at = u32::try_from(self.bin.len()).unwrap();
        let size = (data.len() + 4).next_multiple_of(8);
        self.bin
            .extend_from_slice(&(-i32::try_from(size).unwrap()).to_le_bytes());
        self.bin.extend_from_slice(data);
        self.bin.resize(self.bin.len() + size - 4 - data.len(), 0);
        at
    }

    /// A key named `name` with the keys at `children` and the DWORD `values`; its offset.
    fn key(&mut self, name: &str, children: &[u32], values: &[(&str, u32)]) -> u32 {
        let mut list = b"lh".to_vec();
        list.extend_from_slice(&u16::try_from(children.len()).unwrap().to_le_bytes());
        for child in children {
            list.extend_from_slice(&child.to_le_bytes());
            list.extend_from_slice(&[0; 4]);
        }
        let list = if children.is_empty() {
            u32::MAX
        } else {
            self.cell(&list)
        };
        let vks: Vec<u8> = values
            .iter()
            .flat_map(|(value, data)| {
                let mut vk = b"vk".to_vec();
                vk.extend_from_slice(&u16::try_from(value.len()).unwrap().to_le_bytes());
                vk.extend_from_slice(&0x8000_0004_u32.to_le_bytes());
                vk.extend_from_slice(&data.to_le_bytes());
                vk.extend_from_slice(&4_u32.to_le_bytes());
                vk.extend_from_slice(&[1, 0, 0, 0]);
                vk.extend_from_slice(value.as_bytes());
                self.cell(&vk).to_le_bytes()
            })
            .collect();
        let value_list = if values.is_empty() {
            u32::MAX
        } else {
            self.cell(&vks)
        };
        let mut nk = b"nk".to_vec();
        nk.extend_from_slice(&0x20_u16.to_le_bytes());
        nk.resize(20, 0);
        for field in [
            u32::try_from(children.len()).unwrap(),
            0,
            list,
            u32::MAX,
            u32::try_from(values.len()).unwrap(),
            value_list,
        ] {
            nk.extend_from_slice(&field.to_le_bytes());
        }
        nk.resize(72, 0);
        nk.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
        nk.extend_from_slice(&[0, 0]);
        nk.extend_from_slice(name.as_bytes());
        self.cell(&nk)
    }

    /// A SYSTEM hive that starts control set 2, which holds `rtc` and `hiberboot`; control set 1 holds the opposite
    /// values, so reading the wrong set shows.
    fn system(rtc: u32, hiberboot: u32) -> Vec<u8> {
        let mut h = HiveFile::new();
        let mut set = |name: &str, rtc: u32, hiberboot: u32| {
            let zone = h.key(
                "TimeZoneInformation",
                &[],
                &[("Bias", 0), ("RealTimeIsUniversal", rtc)],
            );
            let power = h.key("Power", &[], &[("HiberbootEnabled", hiberboot)]);
            let session = h.key("Session Manager", &[power], &[]);
            let control = h.key("Control", &[session, zone], &[]);
            h.key(name, &[control], &[])
        };
        let first = set("ControlSet001", 1 - rtc, 1 - hiberboot);
        let second = set("ControlSet002", rtc, hiberboot);
        let select = h.key("Select", &[], &[("Current", 2), ("Default", 2)]);
        let root = h.key("ROOT", &[first, second, select], &[]);
        let size = u32::try_from(h.bin.len().next_multiple_of(4096)).unwrap();
        h.bin[8..12].copy_from_slice(&size.to_le_bytes());
        h.bin.resize(size as usize, 0);
        let mut base = b"regf".to_vec();
        base.extend_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0]);
        base.resize(0x24, 0);
        base.extend_from_slice(&root.to_le_bytes());
        base.extend_from_slice(&size.to_le_bytes());
        base.resize(4096, 0);
        base.extend_from_slice(&h.bin);
        base
    }
}

/// The graphics stack (todo-81) on the recorded machine (the NVIDIA and Intel cards): `doctor` names each gap with
/// its fix (NVIDIA libraries that do not match the loaded module, a missing Vulkan driver, `OpenCL` for the iGPU only
/// through rusticl, switcheroo-control not running) and drops it once the stack is complete.
#[test]
fn doctor_names_each_gap_of_the_graphics_stack_with_its_fix() {
    let m = Machine::new("graphics");
    // the recorded stack is complete: break it as a half-done driver update and a missing package would
    let root = m.dir.join("root");
    std::fs::remove_file(root.join("usr/lib/libnvidia-glcore.so.580.178.04")).unwrap();
    std::fs::remove_file(root.join("usr/share/vulkan/icd.d/intel_icd.json")).unwrap();
    m.set("/usr/lib/libnvidia-glcore.so.570.86.16", "");
    // the user cannot read nvidia-drm's modeset parameter (0400); without modesetting the NVIDIA card has no connectors
    std::fs::remove_file(root.join("sys/module/nvidia_drm/parameters/modeset")).unwrap();
    let connectors = root.join("sys/class/drm/card0-DP-1");
    let kept = m.dir.join("card0-DP-1");
    std::fs::rename(&connectors, &kept).unwrap();
    for other in ["card0-DP-2", "card0-DP-3", "card0-HDMI-A-1", "card0-eDP-2"] {
        std::fs::remove_dir_all(root.join("sys/class/drm").join(other)).unwrap();
    }
    let doctor = || String::from_utf8_lossy(&m.legion(&["doctor"]).stdout).to_string();
    let found = doctor();
    for expected in [
        "- nvidia-drm does not do modesetting",
        "the NVIDIA kernel module is 580.178.04 and its libraries are 570.86.16",
        "/usr/share/vulkan/icd.d/intel_icd.json is missing: install the Intel Vulkan driver (vulkan-intel)",
        "OpenCL reaches the Intel GPU only through Mesa's rusticl",
        "switcheroo-control does not run",
    ] {
        assert!(found.contains(expected), "{expected} missing:\n{found}");
    }

    std::fs::remove_file(m.dir.join("root/usr/lib/libnvidia-glcore.so.570.86.16")).unwrap();
    m.set("/usr/lib/libnvidia-glcore.so.580.178.04", "");
    m.set("/usr/share/vulkan/icd.d/intel_icd.json", "");
    m.set("/etc/OpenCL/vendors/intel.icd", "");
    std::fs::rename(&kept, &connectors).unwrap();
    std::fs::create_dir_all(m.dir.join("home/.config")).unwrap();
    std::fs::write(
        m.dir.join("home/.config/code-flags.conf"),
        "--ozone-platform-hint=auto\n",
    )
    .unwrap();
    Machine::script(&m.dir.join("bin"), "systemctl", "echo active");
    let fixed = doctor();
    for gone in [
        "nvidia-modeset",
        "NVIDIA kernel module",
        "intel_icd",
        "rusticl",
        "switcheroo",
        "XWayland",
    ] {
        assert!(!fixed.contains(gone), "{gone} still found:\n{fixed}");
    }
}

/// The registry checks of the dual boot (D3): `windows status` finds the SYSTEM hive on the NTFS volume of the fstab
/// (the read-only automount this machine has), reads the control set Windows starts with, and names each hazard with
/// its fix in Windows; with the good values it reports none.
#[test]
fn windows_status_reads_the_registry_of_the_mounted_windows_volume() {
    let m = Machine::new("registry");
    Machine::script(
        &m.dir.join("bin"),
        "efibootmgr",
        "printf 'BootCurrent: 0003\\nBoot0004* Windows Boot Manager\\tHD(1,GPT,e8,0x800,0x82000)/\\\\EFI\\\\Microsoft\\\\Boot\\\\bootmgfw.efi\\n'",
    );
    m.set(
        "/etc/fstab",
        "# <file system> <dir> <type> <options> <dump> <pass>\n\
         UUID=1882270A8226EBC6 /mnt/windows\\040system ntfs3 ro,nofail,noauto,x-systemd.automount 0 0",
    );
    let hive = m
        .dir
        .join("root/mnt/windows system/Windows/System32/config/SYSTEM");
    std::fs::create_dir_all(hive.parent().unwrap()).unwrap();

    std::fs::write(&hive, HiveFile::system(0, 1)).unwrap();
    let out = m.legion(&["windows", "status"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "hazards fail the command: {text}");
    assert!(
        text.contains(&format!("registry: {}", hive.display())),
        "{text}"
    );
    assert!(
        text.contains("- Windows keeps the hardware clock in local time")
            && text
                .contains("set the registry value RealTimeIsUniversal (TimeZoneInformation) to 1")
            && !text.contains("reg add"),
        "{text}"
    );
    assert!(
        text.contains("- Fast Startup is on") && text.contains("powercfg /h off"),
        "{text}"
    );

    std::fs::write(&hive, HiveFile::system(1, 0)).unwrap();
    let state: serde_json::Value =
        serde_json::from_str(&m.ok(&["windows", "status", "--json"])).unwrap();
    assert_eq!(state["rtc_utc"], true, "{state}");
    assert_eq!(state["fast_startup"], false, "{state}");
    assert_eq!(state["hazards"], serde_json::json!([]), "{state}");

    std::fs::write(&hive, b"regf but cut short").unwrap();
    let damaged = m.legion(&["windows", "status"]);
    assert!(
        String::from_utf8_lossy(&damaged.stdout).contains("- the Windows registry cannot be read"),
        "{}",
        String::from_utf8_lossy(&damaged.stdout)
    );
}

mod daemon_events {
    use super::*;
    use legionctl_machine::policy::{self, Machine as Planner, Step};
    use legionctl_machine::react::{Event, react};
    use legionctl_machine::sysroot::SysRoot;

    /// The daemon's reaction: every step as a line; file and ledger steps really run against the recorded tree,
    /// commands (systemctl, nvidia-smi) and the power-profiles-daemon profile only as planned lines, so no test touches
    /// this machine.
    fn react_and_write(m: &Machine, event: Event) -> Vec<String> {
        let mut lines = Vec::new();
        with_planner(m, |planner| {
            let steps = react(planner, event).unwrap();
            let files: Vec<Step> = steps
                .iter()
                .filter(|s| !matches!(s, Step::Run(_) | Step::Try(_) | Step::Ppd(_)))
                .cloned()
                .collect();
            policy::execute(planner.root, planner.ledger, &files).unwrap();
            lines = steps.iter().filter_map(policy::describe).collect();
        });
        lines
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
            .position(|l| l == "would set the power-profiles-daemon profile balanced")
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
        let mut lines = Vec::new();
        with_planner(m, |planner| {
            lines = react(planner, event)
                .unwrap()
                .iter()
                .filter(|s| !matches!(s, Step::Run(_) | Step::Try(_) | Step::Ppd(_)))
                .filter_map(policy::describe)
                .collect();
        });
        lines
    }

    /// The EC resets the fan table outside custom mode, and custom mode runs the fans at ~6,000 rpm until a table is
    /// written; the module sets again only a table written since it loaded. So each entry to custom mode writes the
    /// profile's default curve, or the user's curve when there is one, through the module's hwmon files. Without the
    /// module the entry warns and the rest of the plan goes on.
    #[test]
    fn entering_custom_mode_writes_the_users_fan_curve_or_the_default() {
        let m = Machine::new("customentry");
        m.set_table([9; 10]);
        // the shipped profile: the table writes are not verified (todo-17), so custom mode gets no automatic write
        m.set(PROFILE_PATH, "custom");
        react_and_write(&m, Event::PlatformProfile);
        assert_eq!(m.table(), [9; 10]);
        m.verify_fan_calls();
        // Fn+Q to performance: not custom, the table is not touched
        m.set(PROFILE_PATH, "performance");
        react_and_write(&m, Event::PlatformProfile);
        assert_eq!(m.table(), [9; 10]);

        m.set(PROFILE_PATH, "custom");
        let lines = react_and_write(&m, Event::PlatformProfile);
        assert_eq!(m.table(), [1, 2, 3, 4, 5, 6, 7, 8, 9, 10], "{lines:?}");

        m.ledger()
            .set_fan(
                legionctl_machine::ledger::FanKey::Curve,
                Some("2 2 2 3 3 4 6 8 9 10"),
            )
            .unwrap();
        m.set(PROFILE_PATH, "performance");
        react_and_write(&m, Event::PlatformProfile);
        m.set(PROFILE_PATH, "custom");
        react_and_write(&m, Event::PlatformProfile);
        assert_eq!(m.table(), [2, 2, 2, 3, 3, 4, 6, 8, 9, 10]);

        // no module: a warning with the reason, and the mode is still followed
        std::fs::remove_dir_all(m.dir.join("root").join(&WMI_HWMON[1..])).unwrap();
        m.set(PROFILE_PATH, "performance");
        react_and_write(&m, Event::PlatformProfile);
        m.set(PROFILE_PATH, "custom");
        let lines = react_and_write(&m, Event::PlatformProfile);
        assert!(
            lines.contains(
                &"warning: the fan controls are not set again: legionctl_wmi is not loaded: see \
                  docs/design/08-kernel.md"
                    .to_string()
            ),
            "{lines:?}"
        );
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

    /// Every file the power policy writes, set to a value no policy wants: then each plan shows every write it makes.
    fn scramble(m: &Machine) {
        let mut files = vec![
            PROFILE_PATH.to_string(),
            "/sys/devices/system/cpu/intel_pstate/no_turbo".into(),
            "/sys/devices/system/cpu/intel_pstate/max_perf_pct".into(),
            "/sys/module/processor/parameters/ignore_ppc".into(),
            "/proc/sys/vm/swappiness".into(),
            "/proc/sys/net/core/default_qdisc".into(),
            "/proc/sys/net/ipv4/tcp_congestion_control".into(),
        ];
        for i in 0..32 {
            files.push(format!(
                "/sys/devices/system/cpu/cpufreq/policy{i}/scaling_governor"
            ));
            files.push(format!(
                "/sys/devices/system/cpu/cpufreq/policy{i}/energy_performance_preference"
            ));
            files.push(format!(
                "/sys/devices/system/cpu/cpu{i}/power/energy_perf_bias"
            ));
        }
        for file in files {
            if m.file(&file) != "<missing>" {
                m.set(&file, "scrambled");
            }
        }
    }

    /// A step as the migration compares it: everything, except the wording of notes and warnings (their place stays).
    /// Consecutive steps that differ only in the CPU are one line with their count (`policy*/... ×32`).
    fn shown(step: &Step) -> String {
        match step {
            Step::Info(_) => "Info\n".into(),
            Step::Warn(_) => "Warn\n".into(),
            other => {
                let line = format!("{other:?}");
                let mut out = String::new();
                let mut digits_after_prefix = false;
                for (i, c) in line.char_indices() {
                    if c.is_ascii_digit() && digits_after_prefix {
                        continue;
                    }
                    digits_after_prefix = false;
                    out.push(c);
                    let head = &line[..=i];
                    if (head.ends_with("/policy") || head.ends_with("/cpu"))
                        && line[i + 1..].starts_with(|d: char| d.is_ascii_digit())
                    {
                        out.push('*');
                        digits_after_prefix = true;
                    }
                }
                let line = out;
                format!("{line}\n")
            }
        }
    }

    fn grouped(text: &str) -> String {
        let mut out: Vec<(String, usize)> = Vec::new();
        for line in text.lines() {
            match out.last_mut() {
                Some((last, n)) if last == line && !line.starts_with("## ") => *n += 1,
                _ => out.push((line.to_string(), 1)),
            }
        }
        out.into_iter()
            .map(|(line, n)| {
                if n > 1 {
                    format!("{line} ×{n}\n")
                } else {
                    format!("{line}\n")
                }
            })
            .collect()
    }

    /// The plans (every step) and the knobs' wanted values of the recorded machine, as found and with every policy
    /// file scrambled, on AC and on battery, with no Fn+Q choice and with each kind of choice, for each event and for
    /// `power apply` of either source.
    fn migration_plans() -> String {
        use std::fmt::Write;
        let mut out = String::new();
        for (start, scrambled) in [("recorded", false), ("scrambled", true)] {
            for online in ["1", "0"] {
                let m = Machine::new("migration");
                m.set("/sys/class/power_supply/ADP0/online", online);
                if scrambled {
                    scramble(&m);
                }
                m.record(Key::Written, "max-power");
                for choice in [
                    None,
                    Some("performance"),
                    Some("balanced"),
                    Some("custom"),
                    Some("low-power"),
                    Some("no-such-mode"),
                ] {
                    m.ledger().set(Key::Override, choice).unwrap();
                    let name = format!(
                        "{start}, ADP0 online {online}, Fn+Q {}",
                        choice.unwrap_or("none")
                    );
                    with_planner(&m, |p| {
                        for event in [Event::Start, Event::PowerSource, Event::Resume, Event::Tick]
                        {
                            writeln!(out, "## {name}: {event:?}").unwrap();
                            for step in react(p, event).unwrap() {
                                out.push_str(&shown(&step));
                            }
                        }
                        for source in [policy::Source::Ac, policy::Source::Battery] {
                            writeln!(out, "## {name}: apply {}", source.name()).unwrap();
                            let all = policy::Options {
                                force: true,
                                ..policy::Options::default()
                            };
                            for step in p.apply(Some(source), all).unwrap().1 {
                                out.push_str(&shown(&step));
                            }
                        }
                        writeln!(out, "## {name}: knobs").unwrap();
                        for knob in legionctl_machine::knobs::catalog(p).unwrap() {
                            writeln!(out, "{} = {:?}", knob.id, knob.desired).unwrap();
                        }
                    });
                    // Fn+Q to the choice (or back to our own mode)
                    let before = m.file(PROFILE_PATH);
                    m.set(PROFILE_PATH, choice.unwrap_or("max-power"));
                    with_planner(&m, |p| {
                        writeln!(out, "## {name}: PlatformProfile").unwrap();
                        for step in react(p, Event::PlatformProfile).unwrap() {
                            out.push_str(&shown(&step));
                        }
                    });
                    m.set(PROFILE_PATH, &before);
                }
            }
        }
        grouped(&out)
    }

    /// The plans of the shipped layers in each state above, step for step, against the reference in
    /// `tests/fixtures/plans-82wq.txt` (first written by the planner before configsets, todo-74; written again with
    /// `LEGIONCTL_WRITE_PLANS=1` when a plan change is meant: read its diff).
    #[test]
    fn configsets_plan_exactly_the_writes_the_profile_planned() {
        let golden =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/plans-82wq.txt");
        let plans = migration_plans();
        if std::env::var_os("LEGIONCTL_WRITE_PLANS").is_some() {
            std::fs::write(&golden, &plans).unwrap();
        }
        let plans: Vec<&str> = plans.lines().collect();
        let expected = std::fs::read_to_string(golden).unwrap();
        for (line, (want, got)) in expected.lines().zip(&plans).enumerate() {
            assert_eq!(want, *got, "first difference at line {}", line + 1);
        }
        assert_eq!(expected.lines().count(), plans.len());
    }

    /// The daemon's planner on `m`, as the daemon builds it: the installed profile (a test may mark its fan calls
    /// verified) and the configset layers.
    fn with_planner(m: &Machine, f: impl FnOnce(&Planner)) {
        let (profile, _) =
            legionctl_machine::profile::load(Some(&m.dir.join("root").join(INSTALLED_PROFILE)))
                .unwrap();
        let root = SysRoot::new(m.dir.join("root"));
        let ledger = m.ledger();
        let layers = legionctl_machine::configset::load(&root, &profile, &ledger).unwrap();
        f(&Planner {
            root: &root,
            profile: &profile,
            ledger: &ledger,
            ppd_current: Some("performance".into()),
            has_command: &|_| true,
            layers: &layers,
        });
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
    use std::process::{Command, Stdio};
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

    pub(super) use crate::common::process::Kill;

    pub(super) fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
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
        let bus = Kill::spawn(
            Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--nopidfile"])
                .arg(format!("--address=unix:path={}", bus_path.display())),
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
                    denied: &["org.legionctl.gpu.primary"],
                    asked: asked.clone(),
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap();

        // as root, the sensors pin into a BPF filesystem of this machine's own (D7)
        if std::os::unix::fs::MetadataExt::uid(&std::fs::metadata("/proc/self").unwrap()) == 0 {
            let pins = m.dir.join("root/sys/fs/bpf");
            std::fs::create_dir_all(&pins).unwrap();
            assert!(
                Command::new("mount")
                    .args(["-t", "bpf", "bpf"])
                    .arg(&pins)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let daemon = daemon(m, &stand_ins).await;
        (stand_ins, asked, (bus, daemon))
    }

    /// The daemon on the private bus of `m`, once it owns its name.
    pub(super) async fn daemon(m: &Machine, stand_ins: &zbus::Connection) -> Kill {
        let daemon_bin = crate::common::sibling("legionctl-system");
        let mut command = Command::new(daemon_bin);
        command.env_clear();
        // a test's OpenRGB stand-in (`openrgb.addr`); without it the daemon has no OpenRGB
        if let Ok(address) = std::fs::read_to_string(m.dir.join("openrgb.addr")) {
            command.env("LEGIONCTL_OPENRGB", address);
        }
        let daemon = Kill::spawn(
            command
                .env("PATH", m.dir.join("bin"))
                .env("LEGIONCTL_SYSROOT", m.dir.join("root"))
                .env(
                    "DBUS_SYSTEM_BUS_ADDRESS",
                    format!("unix:path={}", m.dir.join("bus").display()),
                )
                .stderr(Stdio::null()),
        );
        let dbus = zbus::fdo::DBusProxy::new(stand_ins).await.unwrap();
        let name = zbus::names::BusName::try_from("org.legionctl.System1").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !dbus.name_has_owner(name.clone()).await.unwrap() {
            assert!(
                Instant::now() < deadline,
                "the daemon did not take its bus name"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        daemon
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
        let lines: Vec<String> = system.call("ApplyPower", &("ac",)).await.unwrap();
        assert!(lines.iter().any(|l| l.contains("max-power")), "{lines:?}");
        assert_eq!(m.file(PROFILE_PATH), "max-power");
        // the daemon owns its name before it handles Start, so a Start signal may come first
        let announced = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let signal = changes.next().await.unwrap();
                let (cause, lines): (String, Vec<String>) = signal.body().deserialize().unwrap();
                if cause == "ApplyPower" {
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
            .call::<_, _, Vec<String>>("SetPrimaryGpu", &("nvidia", false))
            .await
            .unwrap_err();
        assert!(
            refused
                .to_string()
                .contains("not authorized for org.legionctl.gpu.primary"),
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
                    "org.legionctl.gpu.primary".into(),
                    1,
                    false
                ),
            ]
        );
    }

    /// The incident of 2026-10-16 (todo-125): a package upgrade changed the layer files while systemd stopped the
    /// daemon. systemd's stop sends SIGTERM to the unit's processes and kills them after `TimeoutStopSec=10`. The
    /// reload's first step (`cpupower`) is slow and gets the SIGTERM; a start job that the daemon then waits for
    /// queues behind its own stop, so the systemctl stand-in waits as systemd would while the stop is queued (the file
    /// `stopping`). The daemon ends within half of the stop time.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stop_during_a_reload_ends_the_daemon_within_the_stop_time() {
        let m = Machine::new("stop");
        let calls = m.dir.join("calls.log");
        let stopping = m.dir.join("stopping");
        for (name, waits) in [
            ("cpupower", "true"),
            (
                "systemctl",
                "case \" $* \" in *\" --no-block \"*) false;; *) true;; esac",
            ),
        ] {
            Machine::script(
                &m.dir.join("bin"),
                name,
                &format!(
                    "echo \"{name} $*\" >> {calls}; if [ -e {stopping} ] && {waits}; then exec /usr/bin/sleep 60; fi",
                    calls = calls.display(),
                    stopping = stopping.display()
                ),
            );
        }
        let (_stand_ins, _asked, (_bus, mut daemon)) = start(&m).await;
        let log = || std::fs::read_to_string(&calls).unwrap_or_default();
        wait_for("the start's systemctl call", || log().contains("systemctl"));
        let before = log().lines().count();

        std::fs::write(&stopping, "").unwrap();
        m.set("/etc/legionctl/configsets/30-upgrade.jsonc", "{}");
        wait_for("the reload's cpupower call", || {
            log()
                .lines()
                .skip(before)
                .any(|l| l.starts_with("cpupower"))
        });
        let pid = rustix::process::Pid::from_child(&daemon);
        rustix::process::kill_process_group(pid, rustix::process::Signal::TERM).unwrap();
        let stopped = Instant::now();
        while daemon.try_wait().unwrap().is_none() {
            assert!(
                stopped.elapsed() < Duration::from_secs(5),
                "the daemon still runs 5 s after SIGTERM; calls:\n{}",
                log()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// A package upgrade replaces a shipped layer file: it is gone for a moment (todo-125). The daemon applies the
    /// layers once they settled, so the moment without the file changes nothing on the machine.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_layer_file_that_is_gone_for_a_moment_changes_nothing() {
        let m = Machine::new("upgrade");
        let (stand_ins, _asked, _processes) = start(&m).await;
        wait_for("the AC policy at start", || {
            m.file(PROFILE_PATH) == "max-power"
        });
        let system = zbus::Proxy::new(
            &stand_ins,
            "org.legionctl.System1",
            "/org/legionctl/System1",
            "org.legionctl.System1",
        )
        .await
        .unwrap();
        let mut signals = system.receive_signal("Changed").await.unwrap();
        let layer = m
            .dir
            .join("root")
            .join(INSTALLED_LAYERS)
            .join("20-ac.jsonc");
        let aside = m.dir.join("20-ac.jsonc");
        std::fs::rename(&layer, &aside).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::rename(&aside, &layer).unwrap();
        // the daemon owns its name before it handles Start, so a Start signal may come first
        let reloads = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let (cause, lines): (String, Vec<String>) =
                    signals.next().await.unwrap().body().deserialize().unwrap();
                if cause == "Reload" {
                    return lines;
                }
            }
        })
        .await;
        assert!(
            reloads.is_err(),
            "the reload changed the machine: {reloads:?}"
        );
        assert_eq!(m.file(PROFILE_PATH), "max-power");
    }
}

/// The operator on the same private bus as the daemon: machine changes reach the event log, and telemetry gets the
/// root-only part from the daemon.
mod operator_and_daemon {
    use super::daemon_process::{Kill, daemon, start, wait_for};
    use super::*;
    use futures_util::StreamExt;
    use serde_json::Value;

    /// The operator of machine `m` on its private bus (the daemon's).
    pub(super) fn on_bus(m: &Machine, tag: &'static str) -> Operator {
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
        // the MSR at IA32_PACKAGE_THERM_STATUS: thermal now and logged (status bit 0, log bit 1), PROCHOT logged (bit 3)
        let msr = m.dir.join("root/dev/cpu/0/msr");
        std::fs::create_dir_all(msr.parent().unwrap()).unwrap();
        let mut bytes = vec![0u8; 0x1B1 + 8];
        bytes[0x1B1] = 0b1011;
        std::fs::write(&msr, bytes).unwrap();
        m.set("/sys/class/hwmon/hwmon8/name", "coretemp");
        m.set("/sys/class/hwmon/hwmon8/temp1_input", "55000");
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
        assert_eq!(
            sample["throttle_logged"],
            serde_json::json!(["thermal", "PROCHOT"])
        );
        assert_eq!(sample["cpu_temp_c"], 55.0);
        // status and doctor say the daemon's reading (one source; the user process cannot read the MSR live)
        let status = op.ok(&["status"]);
        assert!(
            status.contains("throttle logged: thermal, PROCHOT"),
            "{status}"
        );
        let doctor = String::from_utf8(op.cli(&["doctor"]).stdout).unwrap();
        assert!(
            doctor.contains("- CPU throttling logged: thermal, PROCHOT"),
            "{doctor}"
        );

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
        let lines: Vec<String> = system.call("ApplyPower", &("ac",)).await.unwrap();
        assert!(lines.iter().any(|l| l.contains("max-power")), "{lines:?}");
        let mut logged = Value::Null;
        for _ in 0..100 {
            logged = json(&["events"]);
            if logged["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["kind"] == "machine.apply-power")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let apply = logged["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == "machine.apply-power")
            .unwrap_or_else(|| panic!("not recorded: {logged}"));
        assert_eq!(apply["actor"], "legionctl-system");
        assert_eq!(apply["payload"]["lines"], serde_json::json!(lines));

        // the knob sums up every CPU; its history finds the per-CPU write through the policy* path
        let epp = json(&["knobs", "show", "cpu.epp"]);
        assert_eq!(epp["knob"]["value"], "performance", "{epp}");
        assert_eq!(
            epp["knob"]["desired"], "balance_power",
            "on battery the battery policy is wanted, whatever was applied"
        );
        assert_eq!(epp["knob"]["policy"], "enforce");
        let history = epp["history"]["items"].as_array().unwrap();
        assert_eq!(
            history.last().map(|e| &e["id"]),
            Some(&apply["id"]),
            "{epp}"
        );
        // a knob the daemon did not touch has no history from that apply
        let untouched = json(&["knobs", "show", "firmware.ppt_pl1_spl"]);
        assert!(
            untouched["history"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["id"] != apply["id"]),
            "{untouched}"
        );
    }

    /// A daemon loop that fails (todo-142 F-14 and the observe loop, user decision Q1): when the AC adapter cannot be
    /// read, the plan of a layer change and the state read fail; each is one event on the stream (machine.plan-error,
    /// machine.observe-error, once, not at each retry), a journal line and an open doctor finding with the reason; the
    /// machine keeps its state; the next plan that works closes the finding.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_daemon_loop_is_an_event_and_a_doctor_finding_until_it_works_again() {
        use std::os::unix::fs::PermissionsExt;
        let m = Machine::new("looperr");
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "looperr");
        wait_for("the AC policy at start", || {
            m.file(PROFILE_PATH) == "max-power"
        });
        // the daemon's first plan and the operator's recorder are up: an event before it would not be recorded
        op.ok(&["status"]);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let ac = m.dir.join("root/sys/class/power_supply/ADP0/online");
        let layer = m
            .dir
            .join("root")
            .join(INSTALLED_LAYERS)
            .join("20-ac.jsonc");
        let reload = || {
            let aside = m.dir.join("20-ac.jsonc");
            std::fs::rename(&layer, &aside).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
            std::fs::rename(&aside, &layer).unwrap();
        };
        std::fs::set_permissions(&ac, std::fs::Permissions::from_mode(0o000)).unwrap();
        reload();
        wait_for("the policy error", || {
            m.file("/run/legionctl/policy.error") != "<missing>"
        });
        let error = m.file("/run/legionctl/policy.error");
        assert!(
            error.ends_with(
                ": reading /sys/class/power_supply/ADP0/online: Permission denied (os error 13)"
            ),
            "{error}"
        );
        assert_eq!(m.file(PROFILE_PATH), "max-power");
        let kinds = |kind: &str| -> Vec<Value> {
            let events: Value = serde_json::from_str(&op.ok(&["events", "--json"])).unwrap();
            events["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == kind)
                .cloned()
                .collect()
        };
        wait_for("the loop events", || {
            !kinds("machine.plan-error").is_empty() && !kinds("machine.observe-error").is_empty()
        });
        let reason = m.file("/run/legionctl/policy.error");
        assert_eq!(
            kinds("machine.plan-error").last().unwrap()["payload"]["lines"][0],
            Value::String(reason.clone())
        );
        let doctor = String::from_utf8_lossy(&op.cli(&["doctor"]).stdout).to_string();
        assert!(
            doctor.contains(&format!(
                "legionctl-system cannot carry out the policy, the machine keeps its old state: {reason}"
            )) && doctor.contains("legionctl-system cannot read the machine state that it announces"),
            "{doctor}"
        );
        // a retry with the same reason is no new event
        let (plans, observes) = (
            kinds("machine.plan-error").len(),
            kinds("machine.observe-error").len(),
        );
        reload();
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        assert_eq!(m.file("/run/legionctl/policy.error"), reason);
        assert_eq!(kinds("machine.plan-error").len(), plans);
        assert_eq!(kinds("machine.observe-error").len(), observes);

        std::fs::set_permissions(&ac, std::fs::Permissions::from_mode(0o644)).unwrap();
        reload();
        wait_for("the closed findings", || {
            m.file("/run/legionctl/policy.error") == "<missing>"
                && m.file("/run/legionctl/observe.error") == "<missing>"
        });
        let doctor = String::from_utf8_lossy(&op.cli(&["doctor"]).stdout).to_string();
        assert!(!doctor.contains("legionctl-system cannot"), "{doctor}");
    }

    /// A sweep of PL1 through the operator and the daemon: the stand-in workload scores what the firmware limit is at
    /// each step, so the recommendation shows that each value was really written; the original comes back; the
    /// guards refuse battery and values outside the firmware range.
    #[tokio::test(flavor = "multi_thread")]
    async fn tune_steps_the_firmware_limit_recommends_the_knee_and_restores_it() {
        let m = Machine::new("tune");
        // the sweep's thermal guard reads the package temperature
        m.set("/sys/class/hwmon/hwmon8/name", "coretemp");
        m.set("/sys/class/hwmon/hwmon8/temp1_input", "55000");
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
                    "--seconds",
                    "1",
                    "--cooldown-s",
                    "0",
                    "ppt_pl1_spl",
                    "cpu",
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
        // the recommendation is a generated layer for AC, off until the user enables it
        let tuned = m.file("/var/lib/legionctl/configsets/50-tuned.jsonc");
        assert!(
            tuned.contains("\"power\": \"ac\"") && tuned.contains("\"firmware.ppt_pl1_spl\": 80"),
            "{tuned}"
        );
        let list = op.ok(&["rules", "list"]);
        assert!(
            list.lines().any(|l| l.starts_with("generated/50-tuned")
                && l.ends_with("off: generated: off until `legionctl rules enable 50-tuned`")),
            "{list}"
        );

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
        m.set("/sys/class/hwmon/hwmon8/name", "coretemp");
        m.set("/sys/class/hwmon/hwmon8/temp1_input", "105000");
        Machine::script(&m.dir.join("bin"), "stress-ng", "/usr/bin/sleep 30");
        let started = std::time::Instant::now();
        let hot = op.cli(&[
            "tune",
            "--seconds",
            "30",
            "--cooldown-s",
            "0",
            "ppt_pl1_spl",
            "cpu",
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

    /// A ledger that cannot be opened stops the writer (todo-142 F-7): with a `system.db` that is not a database,
    /// `power apply` fails with the SQLite reason and writes nothing (it cannot know the user's holds), and a reader
    /// (`knobs list`) fails too. Before, the ledger read as empty and the policy wrote over held values.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_ledger_that_cannot_be_opened_stops_the_policy_before_it_writes() {
        const EPP: &str = "/sys/devices/system/cpu/cpufreq/policy3/energy_performance_preference";
        let m = Machine::new("ledger");
        m.set(EPP, "power");
        m.set(
            "/var/lib/legionctl/system.db",
            "not a database, a user's hold was here",
        );
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "ledger");
        let out = op.cli(&["power", "apply", "--source", "ac"]);
        let why = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{why}");
        assert!(
            why.contains("opening /var/lib/legionctl/system.db for writing")
                && why.contains("not a database"),
            "{why}"
        );
        assert_eq!(m.file(EPP), "power", "nothing written");
        let read = op.cli(&["knobs", "list"]);
        let why = String::from_utf8_lossy(&read.stderr);
        assert!(
            !read.status.success() && why.contains("system.db") && why.contains("not a database"),
            "{why}"
        );
    }

    /// The thermal guard must work before the sweep starts (todo-142 F-2): a package temperature that cannot be read
    /// (permission denied) refuses the tune before the first step, with the reason; no limit is written and no
    /// workload runs. Before, the sweep ran with no guard.
    #[tokio::test(flavor = "multi_thread")]
    async fn tune_refuses_before_the_first_step_when_the_temperature_cannot_be_read() {
        use std::os::unix::fs::PermissionsExt;
        let m = Machine::new("blind");
        m.set("/sys/class/hwmon/hwmon8/name", "coretemp");
        m.set("/sys/class/hwmon/hwmon8/temp1_input", "55000");
        let temp = m.dir.join("root/sys/class/hwmon/hwmon8/temp1_input");
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o000)).unwrap();
        let ran = m.dir.join("stress-ng-ran");
        Machine::script(
            &m.dir.join("bin"),
            "stress-ng",
            &format!("touch {}; /usr/bin/sleep 5", ran.display()),
        );
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "blind");
        let out = op.cli(&[
            "tune",
            "--seconds",
            "1",
            "--cooldown-s",
            "0",
            "ppt_pl1_spl",
            "cpu",
            "60",
        ]);
        let text = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{text}");
        assert!(
            text.contains(
                "cannot read the CPU temperature (coretemp): the sweep has no thermal guard"
            ) && text.contains("Permission denied"),
            "{text}"
        );
        assert_eq!(m.file(PL1), "90", "no step ran");
        assert!(!ran.exists(), "no workload ran");
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    /// Without BPF permission (a non-root daemon) the daemon runs on, and each eBPF sensor's error reaches the
    /// sample, the sensors view and doctor.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_sensor_that_cannot_load_is_reported_by_monitor_and_doctor() {
        let m = Machine::new("nobpf");
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "nobpf");
        let out = op.cli(&["monitor", "--interval-ms", "100", "--json"]);
        let sample = &serde_json::from_slice::<Value>(&out.stdout).unwrap()[0];
        let errors = sample["sensor_errors"]
            .as_object()
            .unwrap_or_else(|| panic!("{sample}"));
        assert_eq!(
            errors.keys().collect::<Vec<_>>(),
            [
                "block I/O",
                "power",
                "run-queue",
                "session activity",
                "thermal"
            ],
            "{sample}"
        );
        assert!(
            errors
                .values()
                .all(|e| e.as_str().unwrap().contains("Operation not permitted")),
            "{sample}"
        );
        for reading in ["runq_latency", "cpu_energy", "io_latency", "thermal_trips"] {
            assert_eq!(sample[reading], Value::Null, "{reading}: {sample}");
        }
        let rows: Value = serde_json::from_str(&op.ok(&["sensors", "--json"])).unwrap();
        assert!(
            rows.as_array()
                .unwrap()
                .iter()
                .any(|r| r["source"] == "eBPF power"
                    && r["text"]
                        .as_str()
                        .is_some_and(|t| t.starts_with("not running: loading the power object"))),
            "{rows:#}"
        );
        let doctor = String::from_utf8(op.cli(&["doctor"]).stdout).unwrap();
        for line in [
            "- the eBPF run-queue sensor does not run: loading the runq object: map error",
            "- the eBPF power sensor does not run: loading the power object: map error",
            "- the eBPF block I/O sensor does not run: loading the io object: map error",
            "- the eBPF thermal sensor does not run: loading the thermal object: map error",
            "- the eBPF session activity sensor does not run: loading the exec object: map error",
            "- the eBPF workload sensor does not run: loading the workload object: map error",
        ] {
            assert!(doctor.contains(line), "{line}\n{doctor}");
        }
    }

    /// As root (CI runs it with sudo): the eBPF programs pass the verifier of the running kernel, attach, and the
    /// histogram counts the wakeups of a busy machine. The programs are pinned (D7): a daemon that failed (killed)
    /// and starts again reuses the pins of its build, which go on counting; a stop (SIGTERM) removes the pins.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs root: CI runs it with sudo"]
    async fn as_root_the_runq_sensor_measures_wakeups_and_outlives_a_failed_daemon() {
        use std::os::unix::fs::MetadataExt;

        let m = Machine::new("runq");
        let (stand_ins, _asked, (_bus, first)) = start(&m).await;
        let op = on_bus(&m, "runq");
        let measures = || {
            // wakeups: short sleeps in a few shells
            let load: Vec<_> = (0..4)
                .map(|_| {
                    Kill::spawn(
                        std::process::Command::new("/bin/sh")
                            .args(["-c", "for i in $(seq 200); do /usr/bin/sleep 0.005; done"]),
                    )
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
                classes.values().any(
                    |l| l["count"].as_u64().unwrap() > 0 && l["p99_us"].as_u64().unwrap() >= 2
                ),
                "{sample}"
            );
        };
        measures();
        let pins = m.dir.join("root/sys/fs/bpf/legionctl");
        let pinned = |sensor: &str| {
            std::fs::read_dir(&pins)
                .unwrap()
                .flatten()
                .find(|e| e.file_name().to_string_lossy().starts_with(sensor))
                .unwrap_or_else(|| panic!("no {sensor} pins in {}", pins.display()))
                .path()
        };
        let hist = pinned("runq-").join("runq_hist");
        for pin in ["wakeup", "wakeup_new", "switch_to"] {
            assert!(hist.with_file_name(pin).exists(), "{pin}");
        }
        for pin in ["exec", "execs", "watched"] {
            assert!(pinned("exec-").join(pin).exists(), "{pin}");
        }
        let inode = std::fs::metadata(&hist).unwrap().ino();

        // the daemon fails; the next one finds the pins of its build and loads nothing
        drop(first);
        let second = daemon(&m, &stand_ins).await;
        assert_eq!(std::fs::metadata(&hist).unwrap().ino(), inode);
        measures();

        // a stop removes the pins
        assert!(
            std::process::Command::new("kill")
                .args(["-TERM", &second.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let mut second = second;
        second.wait().unwrap();
        assert!(!pins.exists(), "{}", pins.display());
    }

    /// As root (CI runs it with sudo): a program started in an agent session's scope (a cgroup
    /// `legionctl-session-<id>.scope` of the operator's user) reaches `session activity` with its arguments, through
    /// the eBPF exec sensor and a signal sent to the operator alone; a program outside the scope does not.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs root: CI runs it with sudo"]
    async fn as_root_the_exec_sensor_records_what_a_session_starts() {
        let m = Machine::new("execs");
        let (stand_ins, _asked, _processes) = start(&m).await;
        // the daemon's own reason when the sensor does not run (the verifier's message); asked before the
        // operator starts, so the operator's later call is the watch that counts
        if let Err(e) = stand_ins
            .call_method(
                Some("org.legionctl.System1"),
                "/org/legionctl/System1",
                Some("org.legionctl.System1"),
                "WatchSessions",
                &(),
            )
            .await
        {
            panic!("{e}");
        }
        let op = on_bus(&m, "execs");
        let scope = SessionScope::new("acttest");
        let scope = &scope.0;
        let probe = |inside: bool, word: &str| {
            let enter = if inside {
                format!("echo $$ > {}/cgroup.procs && ", scope.display())
            } else {
                String::new()
            };
            std::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    &format!("{enter}exec /bin/echo legionctl-probe {word}"),
                ])
                .output()
                .unwrap();
        };
        probe(false, "outside");
        // the operator asks to watch at its start; the daemon looks for new scopes each second
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let activity = loop {
            probe(true, "inside");
            let out = op.cli(&["session", "activity", "acttest", "--json"]);
            let rows: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
            // file opens are activity rows too (they can come first): wait for the probe's own start
            let probe_ran = rows["items"].as_array().is_some_and(|r| {
                r.iter().any(|row| {
                    row["argv"] == serde_json::json!(["/bin/echo", "legionctl-probe", "inside"])
                })
            });
            if probe_ran || std::time::Instant::now() > deadline {
                break rows;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        };
        let argvs: Vec<Value> = activity["items"]
            .as_array()
            .unwrap_or_else(|| panic!("no activity: {activity}"))
            .iter()
            .map(|r| r["argv"].clone())
            .collect();
        assert!(
            argvs.contains(&serde_json::json!([
                "/bin/echo",
                "legionctl-probe",
                "inside"
            ])),
            "{activity}"
        );
        assert!(!activity.to_string().contains("outside"), "{activity}");
    }

    /// The workload sensor as root (CI runs it with sudo): a build tool that runs before the daemon starts (found in
    /// /proc) and one that starts later (the exec tracepoint) make the `workload build` layer apply; when the tool
    /// ends (the exit tracepoint) the layer stops applying after the linger time. The verifier of the running kernel
    /// takes both programs.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs root: CI runs it with sudo"]
    async fn as_root_the_workload_sensor_applies_a_layer_while_a_build_runs() {
        let m = Machine::new("workload");
        m.set(
            "/etc/legionctl/configsets/60-build.jsonc",
            r#"{ "when": { "workload": "build" }, "set": { "sysctl.vm.swappiness": "10" } }"#,
        );
        // a program named like a build tool: the sensor goes by the name (comm is the link's name). A link, not a
        // copy: a parallel test's fork can hold a copy's write fd, and exec then fails with ETXTBSY
        let ninja = m.dir.join("bin/ninja");
        std::os::unix::fs::symlink("/bin/sleep", &ninja).unwrap();
        let run = || Kill::spawn(std::process::Command::new(&ninja).arg("60"));
        let workload = || m.file("/run/legionctl/workload");

        let before = run();
        let (stand_ins, _asked, _processes) = start(&m).await;
        wait_for("the build that ran before the daemon", || {
            m.file(SWAPPINESS) == "10" && workload() == "build"
        });
        assert_eq!(m.file("/run/legionctl/workload.error"), "<missing>");
        // other tools hear each change of the kinds (WorkloadChanged)
        let system = zbus::Proxy::new(
            &stand_ins,
            "org.legionctl.System1",
            "/org/legionctl/System1",
            "org.legionctl.System1",
        )
        .await
        .unwrap();
        let mut kinds = system.receive_signal("WorkloadChanged").await.unwrap();
        let mut next = async || -> Vec<String> {
            let signal = tokio::time::timeout(std::time::Duration::from_secs(15), kinds.next())
                .await
                .unwrap()
                .unwrap();
            signal.body().deserialize::<(Vec<String>,)>().unwrap().0
        };
        drop(before);
        wait_for("the end of the build", || {
            m.file(SWAPPINESS) == "150" && workload().is_empty()
        });
        // the start's announcement can come after the subscription
        while !next().await.is_empty() {}

        let _after = run();
        wait_for("a build that starts while the daemon runs", || {
            m.file(SWAPPINESS) == "10" && workload() == "build"
        });
        assert_eq!(next().await, ["build"]);
    }

    const SWAPPINESS: &str = "/proc/sys/vm/swappiness";

    /// An agent session's scope cgroup (`legionctl-session-<id>.scope`) of this user, made for the test and removed
    /// at its end with the folders above it that the test made.
    struct SessionScope(PathBuf, Vec<PathBuf>);

    impl SessionScope {
        fn new(id: &str) -> SessionScope {
            let uid =
                std::os::unix::fs::MetadataExt::uid(&std::fs::metadata("/proc/self").unwrap());
            let scope = PathBuf::from(format!(
                "/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/legionctl-session-{id}.scope"
            ));
            // deepest first, so they go in this order
            let made: Vec<PathBuf> = scope
                .ancestors()
                .take_while(|p| !p.exists())
                .map(Path::to_path_buf)
                .collect();
            std::fs::create_dir_all(&scope).unwrap();
            SessionScope(scope, made)
        }
    }

    impl Drop for SessionScope {
        fn drop(&mut self) {
            for dir in &self.1 {
                let _ = std::fs::remove_dir(dir);
            }
        }
    }

    /// As root (CI runs it with sudo): the power, block I/O and thermal programs pass the verifier of the running
    /// kernel, and a busy loop gets a large share of the CPU energy, under its own name and process id.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs root: CI runs it with sudo"]
    async fn as_root_the_power_sensor_finds_a_busy_loop() {
        let m = Machine::new("power");
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "power");
        // a link, not a copy (a parallel test's fork can hold a copy's write fd): its name is the process's name
        let busy = m.dir.join("bin/lc-busy");
        std::os::unix::fs::symlink("/bin/sh", &busy).unwrap();
        let looping =
            Kill::spawn(std::process::Command::new(&busy).args(["-c", "while :; do :; done"]));
        let out = op.cli(&["monitor", "--interval-ms", "1000", "--json"]);
        let sample = &serde_json::from_slice::<Value>(&out.stdout).unwrap()[0];
        assert_eq!(sample["sensor_errors"], Value::Null, "{sample}");
        assert!(sample["thermal_trips"].is_object(), "{sample}");
        assert!(sample["io_latency"].is_object(), "{sample}");
        let top = sample["cpu_energy"]
            .as_array()
            .unwrap_or_else(|| panic!("{sample}"));
        let ours = top
            .iter()
            .find(|e| e["pid"] == looping.id())
            .unwrap_or_else(|| panic!("the busy loop is not in the top: {sample}"));
        assert_eq!(ours["name"], "lc-busy", "{sample}");
        // one CPU all the time; other tests may run beside it
        assert!(ours["share_percent"].as_f64().unwrap() >= 15.0, "{sample}");
    }

    /// As root (CI runs it with sudo): direct writes to a file give I/O latency counts to the disk that holds it (not
    /// to its partition).
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs root: CI runs it with sudo"]
    async fn as_root_the_block_sensor_counts_the_writes_of_a_dd() {
        use std::os::unix::fs::MetadataExt;

        let m = Machine::new("blockio");
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "blockio");
        // on a disk: /tmp can be a tmpfs
        let scratch = Scratch::new(format!("/var/tmp/legionctl-dd-{}", std::process::id()));
        let file = scratch.join("dd");
        std::fs::write(&file, b"").unwrap();
        // the disk of the file: its partition's sysfs path is .../block/<disk>[/<partition>]
        let dev = std::fs::metadata(&file).unwrap().dev();
        let (major, minor) = (
            ((dev >> 32) & 0xffff_f000) | ((dev >> 8) & 0xfff),
            ((dev >> 12) & 0xffff_ff00) | (dev & 0xff),
        );
        // a partition's sysfs folder is inside its disk's (NVMe paths have no "block" part: CI run 38740561227)
        let node = std::fs::canonicalize(format!("/sys/dev/block/{major}:{minor}")).unwrap();
        let disk_node = if node.join("partition").exists() {
            node.parent().unwrap()
        } else {
            node.as_path()
        };
        let disk = disk_node
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        // writes all through the sample window (the window starts when the operator has started)
        let dd = Kill::spawn(std::process::Command::new("/bin/sh").args([
            "-c",
            &format!(
                "while dd if=/dev/zero of={} bs=1M count=32 oflag=direct 2>/dev/null; do :; done",
                file.display()
            ),
        ]));
        let out = op.cli(&["monitor", "--interval-ms", "1500", "--json"]);
        drop(dd);
        let sample = &serde_json::from_slice::<Value>(&out.stdout).unwrap()[0];
        let count = sample["io_latency"][&disk]["count"]
            .as_u64()
            .unwrap_or_else(|| panic!("no latency for {disk}: {sample}"));
        assert!(count >= 32, "{count} requests on {disk}: {sample}");
    }

    /// As root (CI runs it with sudo): a file that a process in an agent session's scope opens reaches `session
    /// activity`, once however often it is opened, through a signal sent to the operator alone (another connection on
    /// the bus with a match rule for it gets nothing); a file opened outside the scope does not.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs root: CI runs it with sudo"]
    async fn as_root_a_session_file_open_reaches_only_its_owner() {
        let m = Machine::new("opens");
        let (stand_ins, _asked, _processes) = start(&m).await;
        let system = zbus::Proxy::new(
            &stand_ins,
            "org.legionctl.System1",
            "/org/legionctl/System1",
            "org.legionctl.System1",
        )
        .await
        .unwrap();
        let mut overheard = system.receive_signal("SessionOpen").await.unwrap();
        let op = on_bus(&m, "opens");
        let scope = SessionScope::new("opentest");
        let (inside, outside) = (m.dir.join("opened-inside"), m.dir.join("opened-outside"));
        std::fs::write(&inside, "in").unwrap();
        std::fs::write(&outside, "out").unwrap();
        let open = |enter: bool, file: &Path| {
            let enter = if enter {
                format!("echo $$ > {}/cgroup.procs && ", scope.0.display())
            } else {
                String::new()
            };
            std::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    &format!("{enter}exec /bin/cat {0} {0}", file.display()),
                ])
                .output()
                .unwrap();
        };
        open(false, &outside);
        let inside_path = inside.to_str().unwrap();
        let opened = |rows: &Value| -> usize {
            rows["items"]
                .as_array()
                .map_or(0, |r| r.iter().filter(|e| e["path"] == inside_path).count())
        };
        let activity = || -> Value {
            serde_json::from_slice(
                &op.cli(&["session", "activity", "opentest", "--json"])
                    .stdout,
            )
            .unwrap_or(Value::Null)
        };
        // the operator asks to watch at its start; the daemon looks for new scopes each second
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while opened(&activity()) == 0 && std::time::Instant::now() < deadline {
            open(true, &inside);
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        // more opens of the same file after the first one was recorded
        open(true, &inside);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let rows = activity();
        assert_eq!(opened(&rows), 1, "{rows}");
        assert!(!rows.to_string().contains("opened-outside"), "{rows}");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), overheard.next())
                .await
                .is_err(),
            "another connection heard a session's open"
        );
    }

    /// You are the user in front (logind seat0) with your home on machine `m`; the operator runs as you, on the bus of
    /// the daemon. Returns the operator and your layer folder.
    fn seat_user(m: &Machine, tag: &'static str) -> (Operator, PathBuf) {
        let uid = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata("/proc/self").unwrap());
        m.set(
            "/run/systemd/seats/seat0",
            &format!("IS_SEAT0=1\nACTIVE=2\nACTIVE_UID={uid}"),
        );
        m.set(
            "/etc/passwd",
            &format!("root:x:0:0::/root:/bin/sh\nu:x:{uid}:{uid}::/home/u:/bin/sh"),
        );
        let home = m.dir.join("root/home/u");
        std::fs::create_dir_all(&home).unwrap();
        let op = Operator::start(
            tag,
            &[
                ("PATH", m.dir.join("bin").into()),
                ("HOME", home.clone().into()),
                ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
                (
                    "DBUS_SYSTEM_BUS_ADDRESS",
                    format!("unix:path={}", m.dir.join("bus").display()).into(),
                ),
            ],
        );
        (op, home.join(".config/legionctl/configsets"))
    }

    /// The `kernel` condition (todo-84 part 2): a layer for one kernel applies only while that kernel runs (the package
    /// of the running release on the recorded machine), and a layer for a kernel that is not installed is a doctor
    /// finding.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_kernel_layer_applies_only_on_its_kernel() {
        let m = Machine::new("kernelrule");
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let (op, own) = seat_user(&m, "kernelrule");
        assert_eq!(
            m.file("/proc/sys/kernel/osrelease"),
            "7.2.9-2-cachyos-bore-lto"
        );

        let set = |value: &str| {
            op.ok(&[
                "rules",
                "edit",
                "50-plain",
                "--when",
                "kernel=linux-cachyos",
                &format!("sysctl.vm.swappiness={value}"),
            ])
        };
        let out = set("70");
        assert_eq!(m.file(SWAPPINESS), "150", "not on bore-lto: {out}");
        let plan = op.ok(&["plan"]);
        assert!(plan.contains("kernel linux-cachyos-bore-lto"), "{plan}");

        m.set("/proc/sys/kernel/osrelease", "7.2.9-2-cachyos");
        set("71");
        wait_for("the layer on its kernel", || m.file(SWAPPINESS) == "71");

        std::fs::write(
            own.join("60-zen.jsonc"),
            r#"{ "when": { "kernel": "linux-zen" }, "set": { "sysctl.vm.swappiness": "1" } }"#,
        )
        .unwrap();
        let doctor = String::from_utf8(op.cli(&["doctor"]).stdout).unwrap();
        assert!(
            doctor.contains(
                "60-zen.jsonc is not used: condition kernel: no installed kernel linux-zen"
            ),
            "{doctor}"
        );
        let refused = op.cli(&[
            "rules",
            "edit",
            "61-zen",
            "--when",
            "kernel=linux-zen",
            "sysctl.vm.swappiness=2",
        ]);
        assert!(!refused.status.success());
        assert_eq!(m.file(SWAPPINESS), "71");
    }

    /// Configsets end to end (todo-74), through the CLI, the operator and the daemon on the recorded machine: your own
    /// layer is your file and applies at once, without polkit; the "because" chain shows who set a knob; a layer that
    /// fails a check, or is a link, is not used and doctor says why; a power layer is used only with a value for the
    /// other power source too.
    #[tokio::test(flavor = "multi_thread")]
    async fn your_own_layer_applies_at_once_and_a_layer_that_fails_a_check_is_not_used() {
        const PL1: &str = "/sys/class/firmware-attributes/lenovo-wmi-other-0/attributes/ppt_pl1_spl/current_value";
        let m = Machine::new("ownrules");
        let (_stand_ins, asked, _processes) = start(&m).await;
        let (op, own) = seat_user(&m, "ownrules");
        assert_eq!(m.file(SWAPPINESS), "150");

        let out = op.ok(&["rules", "edit", "50-mine", "sysctl.vm.swappiness=60"]);
        assert_eq!(m.file(SWAPPINESS), "60", "{out}");
        assert!(own.join("50-mine.jsonc").exists());
        let show = op.ok(&["knobs", "show", "sysctl.vm.swappiness"]);
        assert!(
            show.contains(
                "because:\n  shipped/10-base (always): 150\n  user/50-mine (always): 60  <- wins\n  hold: none"
            ),
            "{show}"
        );

        std::fs::write(
            own.join("60-bad.jsonc"),
            r#"{ "when": { "weather": "rain" }, "set": { "sysctl.vm.swappiness": "1" } }"#,
        )
        .unwrap();
        std::os::unix::fs::symlink(m.dir.join("root/etc/passwd"), own.join("70-link.jsonc"))
            .unwrap();
        op.ok(&["rules", "edit", "50-mine", "sysctl.vm.swappiness=61"]);
        assert_eq!(m.file(SWAPPINESS), "61");
        let doctor = String::from_utf8(op.cli(&["doctor"]).stdout).unwrap();
        assert!(
            doctor.contains(
                "60-bad.jsonc is not used: unknown condition weather (power, fnq, workload, kernel)"
            ),
            "{doctor}"
        );
        assert!(
            doctor.contains("70-link.jsonc is not used: cannot open it (a link is not read)"),
            "{doctor}"
        );

        let ac_only = op.ok(&[
            "rules",
            "edit",
            "55-ac",
            "--when",
            "power=ac",
            "firmware.ppt_pl1_spl=95",
        ]);
        assert!(
            ac_only.contains("is not used: firmware.ppt_pl1_spl is set for ac only"),
            "{ac_only}"
        );
        assert_eq!(
            m.file(PL1),
            "90",
            "a layer for one power source only is not used"
        );
        op.ok(&[
            "rules",
            "edit",
            "55-battery",
            "--when",
            "power=battery",
            "firmware.ppt_pl1_spl=45",
        ]);
        assert_eq!(m.file(PL1), "95", "on AC, with a value for battery too");
        assert_eq!(
            asked.lock().unwrap().len(),
            0,
            "your own layers need no polkit"
        );
    }

    /// This computer's forbidden entry adds to the list: you cannot remove it (only your own), and it stays.
    fn a_computers_forbidden_entry_stays(op: &Operator) {
        op.ok(&[
            "config",
            "add",
            "actions.forbid",
            "op:knobs.list",
            "--reason",
            "Site rule",
            "--scope",
            "computer",
        ]);
        let refused = op.cli(&["config", "remove", "actions.forbid", "op:knobs.list"]);
        assert!(
            !refused.status.success()
                && String::from_utf8_lossy(&refused.stderr)
                    .contains("op:knobs.list is not one of the entries for you"),
            "{refused:?}"
        );
        let site = serde_json::from_str::<Value>(&op.ok(&[
            "config",
            "settings",
            "--key",
            "actions.forbid",
            "--json",
        ]))
        .unwrap()["items"][0]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["pattern"] == "op:knobs.list")
            .map(|e| e["from"].clone());
        assert_eq!(
            site,
            Some(serde_json::json!("computer")),
            "the computer's entry stays"
        );
    }

    /// A setting for this computer goes only through the system service: polkit asks for each change, the service
    /// writes /etc/legionctl/managed.jsonc, your own value applies over it, the history keeps who changed what for
    /// which layer, and a setting that this computer locks is refused for you and changes for the computer only.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_computer_setting_goes_through_the_system_service_with_polkit_and_its_history() {
        let m = Machine::new("cfgsys");
        let (_stand_ins, asked, _processes) = start(&m).await;
        let op = on_bus(&m, "cfgsys");
        let row = |key: &str| -> Value {
            serde_json::from_str::<Value>(&op.ok(&["config", "settings", "--key", key, "--json"]))
                .unwrap()["items"][0]
                .clone()
        };
        let said = op.ok(&[
            "config",
            "set",
            "dossier.nudge_turns",
            "9",
            "--scope",
            "computer",
        ]);
        assert_eq!(
            said.trim(),
            "Replies before a notes reminder: set for this computer. In use now: 9."
        );
        assert!(
            m.file("/etc/legionctl/managed.jsonc")
                .contains("\"nudge_turns\": 9"),
            "{}",
            m.file("/etc/legionctl/managed.jsonc")
        );
        assert_eq!(row("dossier.nudge_turns")["from"], "computer");
        op.ok(&["config", "set", "dossier.nudge_turns", "4"]);
        let turns = row("dossier.nudge_turns");
        assert_eq!(
            (turns["value"].clone(), turns["from"].clone()),
            (serde_json::json!(4), serde_json::json!("user"))
        );

        op.ok(&[
            "config",
            "set",
            "config.overrides",
            r#"{"dossier.gate": {"scope": ["computer"]}}"#,
            "--scope",
            "computer",
        ]);
        let refused = op.cli(&["config", "set", "dossier.gate", "false"]);
        let why = String::from_utf8_lossy(&refused.stderr);
        assert!(
            !refused.status.success()
                && why.contains(
                    "Agents must use the notes (dossier.gate) is locked: only this computer's administrator can change it"
                ),
            "{why}"
        );
        op.ok(&[
            "config",
            "set",
            "dossier.gate",
            "false",
            "--scope",
            "computer",
        ]);
        assert_eq!(row("dossier.gate")["value"], false);
        assert_eq!(row("dossier.gate")["locked"], true);

        let history: Value =
            serde_json::from_str(&op.ok(&["config", "history", "--json"])).unwrap();
        let rows: Vec<(&str, &str)> = history["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (c["key"].as_str().unwrap(), c["scope"].as_str().unwrap()))
            .collect();
        assert_eq!(
            rows,
            [
                ("dossier.gate", "computer"),
                ("config.overrides", "computer"),
                ("dossier.nudge_turns", "user"),
                ("dossier.nudge_turns", "computer")
            ]
        );
        let first = history["items"][3]["id"].as_i64().unwrap().to_string();
        op.ok(&["config", "revert", &first]);
        assert!(
            !m.file("/etc/legionctl/managed.jsonc")
                .contains("nudge_turns"),
            "{}",
            m.file("/etc/legionctl/managed.jsonc")
        );
        a_computers_forbidden_entry_stays(&op);
        let actions: Vec<String> = asked.lock().unwrap().iter().map(|a| a.1.clone()).collect();
        assert_eq!(
            actions, ["org.legionctl.settings"; 5],
            "the computer's changes ask; yours do not"
        );
    }

    /// A system layer that an admin writes applies through inotify, and its lock beats your layer; a disabled shipped
    /// layer stays off; reset removes your layers, and with `--system` the system layers and the on/off states: the
    /// shipped state. Each system change asks polkit.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_system_layer_locks_a_knob_and_reset_gives_the_shipped_state_back() {
        let m = Machine::new("sysrules");
        let (_stand_ins, asked, _processes) = start(&m).await;
        let (op, own) = seat_user(&m, "sysrules");
        op.ok(&["rules", "edit", "50-mine", "sysctl.vm.swappiness=60"]);
        assert_eq!(m.file(SWAPPINESS), "60");

        m.set(
            "/etc/legionctl/configsets/40-site.jsonc",
            r#"{ "set": { "sysctl.vm.swappiness": "100" }, "locked": ["sysctl.vm.swappiness"] }"#,
        );
        wait_for("the system layer", || m.file(SWAPPINESS) == "100");
        let doctor = String::from_utf8(op.cli(&["doctor"]).stdout).unwrap();
        assert!(
            doctor.contains(
                "50-mine.jsonc is not used: sysctl.vm.swappiness is locked by system/40-site"
            ),
            "{doctor}"
        );

        op.ok(&["rules", "disable", "10-base"]);
        let list = op.ok(&["rules", "list"]);
        assert!(
            list.lines().any(|l| l.starts_with("shipped/10-base")
                && l.ends_with("off: disabled (legionctl rules enable turns it on)")),
            "{list}"
        );

        op.ok(&["rules", "reset"]);
        assert!(!own.join("50-mine.jsonc").exists());
        assert_eq!(m.file(SWAPPINESS), "100");
        op.ok(&["rules", "reset", "--system"]);
        assert_eq!(m.file(SWAPPINESS), "150");
        assert_eq!(
            m.file("/etc/legionctl/configsets/40-site.jsonc"),
            "<missing>"
        );
        let list = op.ok(&["rules", "list"]);
        assert!(
            list.lines()
                .any(|l| l.starts_with("shipped/10-base") && l.ends_with("applies now")),
            "{list}"
        );
        let actions: Vec<String> = asked.lock().unwrap().iter().map(|a| a.1.clone()).collect();
        assert_eq!(
            actions, ["org.legionctl.rules"; 3],
            "disable, the system layer, the state"
        );
    }

    /// Safe mode reaches the desktop: the operator's session module finds the daemon's safe-mode reason and sends one
    /// desktop notification (a stand-in `org.freedesktop.Notifications` on the private bus, which is also the session
    /// bus here) with the reason and what to do. This desktop shows the body as plain text (no `body-markup`), so the
    /// reason's `<` and `&` reach it as they are.
    #[tokio::test(flavor = "multi_thread")]
    async fn safe_mode_becomes_one_desktop_notification() {
        let m = Machine::new("safenote");
        let (_stand_ins, _asked, (_bus, daemon)) = start(&m).await;
        // the daemon failed (it is gone) and legionctl-safe left its reason
        drop(daemon);
        m.set(
            "/run/legionctl/safe-mode",
            "set: platform_profile <balanced> & fan auto\n",
        );
        let address = format!("unix:path={}", m.dir.join("bus").display());
        let notices = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let desktop = zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .name("org.freedesktop.Notifications")
            .unwrap()
            .build()
            .await
            .unwrap();
        let seen = notices.clone();
        let replies = desktop.clone();
        tokio::spawn(async move {
            let mut calls = zbus::MessageStream::from(&replies);
            while let Some(Ok(call)) = futures_util::StreamExt::next(&mut calls).await {
                if call
                    .header()
                    .member()
                    .is_some_and(|m| m.as_str() == "GetCapabilities")
                {
                    // a desktop that shows the body as plain text
                    replies
                        .reply(&call.header(), &(vec!["actions"],))
                        .await
                        .unwrap();
                } else if call
                    .header()
                    .member()
                    .is_some_and(|m| m.as_str() == "Notify")
                {
                    let (_, _, _, summary, body, ..): crate::desktop::Notify =
                        call.body().deserialize().unwrap();
                    seen.lock().unwrap().push((summary, body));
                    replies.reply(&call.header(), &(1u32,)).await.unwrap();
                }
            }
        });
        let op = Operator::start(
            "safenote",
            &[
                ("PATH", m.dir.join("bin").into()),
                ("HOME", m.dir.join("home").into()),
                ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
                ("DBUS_SYSTEM_BUS_ADDRESS", address.clone().into()),
                ("DBUS_SESSION_BUS_ADDRESS", address.into()),
            ],
        );
        // the first call starts the socket-activated operator, and with it the session module
        op.ok(&["load"]);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while notices.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert_eq!(
            *notices.lock().unwrap(),
            [(
                "legionctl: safe mode".to_string(),
                // the daemon's reason once, with no shell commands (EV-review-11 U-15); as it is for a desktop
                // that shows the body as plain text
                "set: platform_profile <balanced> & fan auto".to_string()
            )]
        );
        drop(desktop);
    }

    /// Supervision: a resident module that stops after it ran ends the operator with an error, so systemd starts it
    /// again (here the bus goes away under the event recorder and the session module).
    #[tokio::test(flavor = "multi_thread")]
    async fn the_operator_exits_with_an_error_when_a_running_module_stops() {
        let m = Machine::new("supervise");
        let (_stand_ins, _asked, (bus, _daemon)) = start(&m).await;
        let address = format!("unix:path={}", m.dir.join("bus").display());
        let mut op = Operator::start(
            "supervise",
            &[
                ("PATH", m.dir.join("bin").into()),
                ("HOME", m.dir.join("home").into()),
                ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
                ("DBUS_SYSTEM_BUS_ADDRESS", address.clone().into()),
                ("DBUS_SESSION_BUS_ADDRESS", address.into()),
            ],
        );
        op.ok(&["load"]);
        // the recorder subscribes to the daemon's signals; then the bus stops
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        assert!(op.child.try_wait().unwrap().is_none(), "the operator runs");
        drop(bus);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let status = loop {
            if let Some(status) = op.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the operator still runs without its bus"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        assert!(!status.success(), "{status}");
    }

    /// Undo (D1, decided 2026-10-13: a user override layer): `undo <event>` turns the writes of a machine change back
    /// to what they replaced and holds them there; the policy leaves held files alone (an apply says so) until
    /// `knobs release`, which gives them back to the policy. Real daemon on the private bus, polkit allows.
    #[tokio::test(flavor = "multi_thread")]
    async fn undo_holds_what_a_change_replaced_until_released() {
        let m = Machine::new("undo");
        // on battery, so that applying the AC policy changes the CPUs' EPP
        m.set("/sys/class/power_supply/ADP0/online", "0");
        let epp0 = "/sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference";
        let recorded = m.file(epp0);
        let (stand_ins, _asked, _processes) = start(&m).await;
        // the daemon applies the battery policy after it owns its name
        wait_for("the battery policy at start", || m.file(epp0) != recorded);
        let op = on_bus(&m, "undo");
        let before = m.file(epp0);
        op.ok(&["events"]);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        op.ok(&["power", "apply", "--source", "ac"]);
        let applied = m.file(epp0);
        assert_ne!(applied, before, "the AC policy changes the EPP");
        let json = |args: &[&str]| -> Value {
            serde_json::from_str(&op.ok(&[args, &["--json"]].concat())).unwrap()
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let event = loop {
            let found = json(&["events"])["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["kind"] == "machine.apply-power")
                .cloned();
            if let Some(e) = found {
                break e;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the change was not recorded"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        assert!(
            event["payload"]["lines"]
                .to_string()
                .contains(&format!("(was {before})")),
            "a write says what it replaced: {event}"
        );

        op.ok(&["undo", &event["id"].to_string()]);
        assert_eq!(m.file(epp0), before, "undone");
        assert_eq!(
            json(&["knobs", "show", "cpu.epp"])["knob"]["held"],
            before.as_str()
        );
        let again = op.ok(&["power", "apply", "--source", "ac"]);
        assert!(again.contains("you hold it"), "{again}");
        assert_eq!(m.file(epp0), before, "the policy leaves a held file alone");

        op.ok(&["knobs", "release", "cpu.epp"]);
        let knob = json(&["knobs", "show", "cpu.epp"])["knob"].clone();
        assert_eq!(knob["held"], Value::Null);
        assert_eq!(
            m.file(epp0),
            knob["desired"].as_str().unwrap(),
            "released: the policy of the current power source (battery) takes it again"
        );

        // the daemon writes as root: a path that is no knob and no file of the policy is refused
        let refused = zbus::Proxy::new(
            &stand_ins,
            "org.legionctl.System1",
            "/org/legionctl/System1",
            "org.legionctl.System1",
        )
        .await
        .unwrap()
        .call::<_, _, Vec<String>>(
            "Hold",
            &(std::collections::BTreeMap::from([(
                "/etc/passwd".to_string(),
                "x".to_string(),
            )]),),
        )
        .await
        .unwrap_err();
        assert!(refused.to_string().contains("neither a knob"), "{refused}");
    }

    /// The PCI side of the recorded machine for VFIO: the RTX 4080 and its audio function (IOMMU group 17, on nvidia
    /// and `snd_hda_intel`, drawn on by `gnome-shell`), and the Windows `NVMe` controller (group 18, a partition
    /// mounted), with a stand-in kernel: a write of a device to `drivers_probe` moves its `driver` link to what
    /// `driver_override` names (or back to its own driver) and makes `/dev/vfio/<group>`, as the kernel does.
    pub(super) fn pci_machine(m: &Machine) -> std::thread::JoinHandle<()> {
        let root = m.dir.join("root");
        let link = |from: &str, to: &str| {
            let at = root.join(from);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            let _ = std::fs::remove_file(&at);
            std::os::unix::fs::symlink(to, at).unwrap();
        };
        let devices = [
            ("0000:01:00.0", "17", "nvidia", "0x030000", "0x10de"),
            ("0000:01:00.1", "17", "snd_hda_intel", "0x040300", "0x10de"),
            ("0000:06:00.0", "18", "nvme", "0x010802", "0x1c5c"),
        ];
        for (device, group, driver, class, vendor) in devices {
            let dir = format!("/sys/bus/pci/devices/{device}");
            m.set(&format!("{dir}/class"), class);
            m.set(&format!("{dir}/vendor"), vendor);
            m.set(&format!("{dir}/device"), "0x27e0");
            m.set(&format!("{dir}/driver_override"), "(null)");
            link(
                &format!("{}/iommu_group", &dir[1..]),
                &format!("../../../../kernel/iommu_groups/{group}"),
            );
            link(
                &format!("{}/driver", &dir[1..]),
                &format!("../../drivers/{driver}"),
            );
            m.set(
                &format!("/sys/kernel/iommu_groups/{group}/devices/{device}"),
                "",
            );
            m.set(&format!("/sys/bus/pci/drivers/{driver}/unbind"), "");
        }
        m.set("/sys/bus/pci/drivers/vfio-pci/unbind", "");
        m.set("/sys/bus/pci/drivers_probe", "");
        m.set("/sys/bus/pci/devices/0000:01:00.0/drm/card0/dev", "226:0");
        m.set(
            "/sys/bus/pci/devices/0000:06:00.0/nvme/nvme0/nvme0n1/size",
            "1",
        );
        m.set("/sys/block/nvme0n1/nvme0n1p3/partition", "3");
        m.set("/dev/dri/card0", "");
        m.set("/dev/nvidia0", "");
        m.set("/proc/4242/comm", "gnome-shell");
        link("proc/4242/fd/7", "/dev/dri/card0");
        m.set(
            "/proc/1/mounts",
            "/dev/nvme1n1p2 / btrfs rw 0 0\n/dev/nvme0n1p3 /mnt/windows-system ntfs3 ro 0 0",
        );
        m.set("/proc/swaps", "Filename Type Size Used Priority");
        let kernel = root.clone();
        std::thread::spawn(move || {
            let own: std::collections::BTreeMap<&str, (&str, &str)> = devices
                .iter()
                .map(|(d, g, drv, _, _)| (*d, (*g, *drv)))
                .collect();
            while kernel.exists() {
                let probe = kernel.join("sys/bus/pci/drivers_probe");
                let device = std::fs::read_to_string(&probe)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if let Some((group, driver)) = own.get(device.as_str()) {
                    let dir = kernel.join(format!("sys/bus/pci/devices/{device}"));
                    let wanted =
                        std::fs::read_to_string(dir.join("driver_override")).unwrap_or_default();
                    let to = if wanted.trim() == "vfio-pci" {
                        "vfio-pci"
                    } else {
                        driver
                    };
                    let _ = std::fs::remove_file(dir.join("driver"));
                    std::os::unix::fs::symlink(format!("../../drivers/{to}"), dir.join("driver"))
                        .unwrap();
                    if to == "vfio-pci" {
                        std::fs::create_dir_all(kernel.join("dev/vfio")).unwrap();
                        std::fs::write(kernel.join(format!("dev/vfio/{group}")), "").unwrap();
                    }
                    std::fs::write(&probe, "").unwrap();
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        })
    }

    /// VFIO (decided 2026-10-14): `vfio status` shows each GPU and `NVMe` controller with its group and what uses it; a
    /// VM gets only a whole group, nothing the host uses (a process on the GPU, a mounted partition), and a memory lock
    /// only for a process of the caller; then the group is bound to vfio-pci (the kernel's driver link checked),
    /// `/dev/vfio/<group>` is the caller's, the runner may lock the VM's memory, and `vfio unbind` gives it back.
    /// What uses a device is the host's mount table (PID 1's), not the daemon's own: the binds of its sandbox (live
    /// r340: "/dev/nvme1n1p2 is mounted at /etc/greetd, /root, /var/lib/legionctl", EV-review-12) are no use.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_whole_unused_iommu_group_goes_to_a_vm_and_back() {
        let m = Machine::new("vfio");
        let _kernel = pci_machine(&m);
        m.set(
            "/proc/self/mounts",
            "/dev/nvme1n1p2 / btrfs rw 0 0\n/dev/nvme0n1p3 /mnt/windows-system ntfs3 ro 0 0\n\
             /dev/nvme0n1p3 /var/lib/legionctl ntfs3 rw 0 0",
        );
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "vfio");
        let status = op.ok(&["vfio", "status"]);
        for expected in [
            "gpu 0000:01:00.0 10de:27e0 group 17 (0000:01:00.0 0000:01:00.1) driver nvidia: in use",
            "- gnome-shell (pid 4242) uses /dev/dri/card0",
            "nvme 0000:06:00.0 1c5c:27e0 group 18 (0000:06:00.0) driver nvme: in use",
            "- /dev/nvme0n1p3 is mounted at /mnt/windows-system",
        ] {
            assert!(status.contains(expected), "{expected} missing:\n{status}");
        }
        assert!(
            !status.contains("/var/lib/legionctl"),
            "a sandbox bind as a use:\n{status}"
        );
        let refused = |args: &[&str], why: &str| {
            let out = op.cli(&[&["vfio", "bind"], args].concat());
            let text = String::from_utf8_lossy(&out.stderr).to_string();
            assert!(
                !out.status.success() && text.contains(why),
                "{why} missing:\n{text}"
            );
        };
        let gpu = ["0000:01:00.0", "0000:01:00.1"];
        refused(
            &["0000:01:00.0"],
            "IOMMU group 17 is 0000:01:00.0 0000:01:00.1, not 0000:01:00.0",
        );
        refused(&gpu, "gnome-shell (pid 4242) uses /dev/dri/card0");
        refused(
            &["0000:06:00.0"],
            "/dev/nvme0n1p3 is mounted at /mnt/windows-system",
        );
        assert_eq!(
            m.file("/sys/bus/pci/devices/0000:01:00.0/driver_override"),
            "(null)",
            "a refusal writes nothing"
        );

        // the desktop left the GPU; a runner of another user may not get the lock, so nothing stays bound
        std::fs::remove_file(m.dir.join("root/proc/4242/fd/7")).unwrap();
        m.set("/proc/1/status", "Name:\tsystemd\nUid:\t0\t0\t0\t0");
        refused(
            &[&gpu[..], &["--pid", "1", "--memlock-mib", "1"]].concat(),
            "process 1 is not a process of uid",
        );
        let driver = |d: &str| {
            std::fs::read_link(m.dir.join(format!("root/sys/bus/pci/devices/{d}/driver"))).unwrap()
        };
        assert_eq!(
            driver("0000:01:00.0"),
            Path::new("../../drivers/nvidia"),
            "given back after the refusal"
        );

        let runner = Kill::spawn(std::process::Command::new("sleep").arg("30"));
        let uid = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata("/proc/self").unwrap());
        m.set(
            &format!("/proc/{}/status", runner.id()),
            &format!("Name:\tsleep\nUid:\t{uid}\t{uid}\t{uid}\t{uid}"),
        );
        let bound = op.ok(&[
            &["vfio", "bind"],
            &gpu[..],
            &["--pid", &runner.id().to_string(), "--memlock-mib", "1"],
        ]
        .concat());
        for expected in [
            "0000:01:00.0: nvidia -> vfio-pci",
            "0000:01:00.1: snd_hda_intel -> vfio-pci",
            &format!("/dev/vfio/17: uid {uid}"),
            &format!("process {}: may lock 1 MiB", runner.id()),
        ] {
            assert!(bound.contains(expected), "{expected} missing:\n{bound}");
        }
        assert_eq!(driver("0000:01:00.0"), Path::new("../../drivers/vfio-pci"));
        let limits = std::fs::read_to_string(format!("/proc/{}/limits", runner.id())).unwrap();
        assert!(
            limits
                .lines()
                .any(|l| l.starts_with("Max locked memory") && l.contains("1048576")),
            "{limits}"
        );
        drop(runner);

        let back = op.ok(&[&["vfio", "unbind"], &gpu[..]].concat());
        assert!(back.contains("0000:01:00.0: vfio-pci -> nvidia"), "{back}");
        assert_eq!(
            driver("0000:01:00.1"),
            Path::new("../../drivers/snd_hda_intel")
        );
    }

    /// The host-use guard needs the mount table (todo-142 F-1, F-6): when `/proc/1/mounts` cannot be read
    /// (permission denied), the bind of the `NVMe` controller is refused with the reason, nothing is written, and its
    /// driver stays; it never reads as "nothing mounted".
    #[tokio::test(flavor = "multi_thread")]
    async fn the_host_use_guard_refuses_when_the_mount_table_cannot_be_read() {
        use std::os::unix::fs::PermissionsExt;
        let m = Machine::new("vfio-unread");
        let _kernel = pci_machine(&m);
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "vfio-unread");
        let mounts = m.dir.join("root/proc/1/mounts");
        std::fs::set_permissions(&mounts, std::fs::Permissions::from_mode(0o000)).unwrap();
        let out = op.cli(&["vfio", "bind", "0000:06:00.0"]);
        let text = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{text}");
        assert!(
            text.contains("cannot read /proc/1/mounts: the host-use check cannot run")
                && text.contains("Permission denied"),
            "{text}"
        );
        assert_eq!(
            m.file("/sys/bus/pci/devices/0000:06:00.0/driver_override"),
            "(null)",
            "a refusal writes nothing"
        );
        assert_eq!(
            std::fs::read_link(m.dir.join("root/sys/bus/pci/devices/0000:06:00.0/driver")).unwrap(),
            Path::new("../../drivers/nvme")
        );
        std::fs::set_permissions(&mounts, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    /// A machine read that fails is the state of that value, with its cause, never a silent default (todo-142 F-26,
    /// F-29, F-31, F-32, F-34): with the IOMMU off the NVIDIA GPU still has its power state, and its owner is unknown
    /// with the reason (before, the GPU vanished); a systemctl without a bus is a doctor check that cannot run (before,
    /// "unknown" counted as disabled); a failing dkms is each kernel's DKMS state; an unreadable layer folder is a
    /// layer fault (before, no layers); no /proc/meminfo fails the load (before, all memory was free).
    #[test]
    fn a_machine_read_that_fails_is_the_state_of_its_value_with_the_cause() {
        use std::os::unix::fs::PermissionsExt;
        let m = Machine::new("causes");
        let _kernel = pci_machine(&m);
        let root = m.dir.join("root");
        let bin = m.dir.join("bin");
        std::fs::remove_file(root.join("sys/bus/pci/devices/0000:01:00.0/iommu_group")).unwrap();
        Machine::script(
            &bin,
            "systemctl",
            "echo 'Failed to connect to bus: No such file or directory' >&2; exit 1",
        );
        Machine::script(&bin, "dkms", "echo 'dkms: broken tree' >&2; exit 3");
        let layers = root.join("etc/legionctl/configsets");
        std::fs::create_dir_all(&layers).unwrap();
        std::fs::set_permissions(&layers, std::fs::Permissions::from_mode(0o000)).unwrap();
        let op = m.operator();
        let json = |args: &[&str]| -> Value {
            serde_json::from_str(&op.ok(&[args, &["--json"]].concat())).unwrap()
        };

        let gpu = op.ok(&["gpu", "status"]);
        assert!(gpu.contains("NVIDIA GPU power: "), "{gpu}");
        let list = json(&["resources", "list"]);
        let nvidia = list["resources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "gpu:nvidia")
            .unwrap_or_else(|| panic!("no gpu:nvidia in {list}"));
        assert_eq!(
            nvidia["state"],
            "unknown: the NVIDIA GPU 0000:01:00.0 has no IOMMU group (is the IOMMU on?)"
        );
        assert_eq!(nvidia["actions"], serde_json::json!([]));

        let doctor = String::from_utf8_lossy(&op.cli(&["doctor"]).stdout).to_string();
        assert!(
            doctor.contains("- the conflict check could not run (systemctl is-active ")
                && doctor
                    .contains("gave no state: Failed to connect to bus: No such file or directory"),
            "{doctor}"
        );
        let kernels = json(&["kernel", "list"]);
        assert!(
            kernels["kernels"][0]["dkms"]
                .as_array()
                .unwrap()
                .contains(&Value::String(
                    "nvidia: unknown: dkms status failed (exit status: 3): dkms: broken tree"
                        .into()
                )),
            "{kernels}"
        );
        let rules = op.ok(&["rules", "list"]);
        assert!(
            rules.contains("cannot open the folder (a link is not read)")
                && rules.contains("Permission denied"),
            "{rules}"
        );
        std::fs::set_permissions(&layers, std::fs::Permissions::from_mode(0o755)).unwrap();

        std::fs::remove_file(root.join("proc/meminfo")).unwrap();
        let out = op.cli(&["load"]);
        assert!(
            !out.status.success()
                && String::from_utf8_lossy(&out.stderr).contains("/proc/meminfo is missing"),
            "{out:?}"
        );
    }

    /// Reads as the user on the live machine (todo-142, EV-validation-read-ops-r260): status, gpu status, knobs list,
    /// plan and resources list failed on one value. Now a RAPL constraint whose time window the kernel has no data for
    /// (ENODATA, PL4 `peak_power`; a recording keeps that read as `<file>.errno`, and `legionctl record` writes it
    /// again) has no window and keeps its limit; processes whose files the user may not read (the
    /// user's own `systemd --user`, not dumpable: its fds list but their links are denied; another user's: its fd
    /// folder is denied) are counted and named as not visible, while a visible process still shows; the daemon's
    /// host-use guard, which must see every process, refuses with that count.
    #[tokio::test(flavor = "multi_thread")]
    async fn reads_as_the_user_count_the_processes_they_cannot_see_and_keep_a_value_without_data() {
        use std::os::unix::fs::PermissionsExt;
        let m = Machine::new("notvisible");
        let _kernel = pci_machine(&m);
        let root = m.dir.join("root");
        let zone = "/sys/class/powercap/intel-rapl-mmio:0";
        m.set(&format!("{zone}/name"), "package-0");
        for (i, name, uw) in [
            (0, "long_term", "140000000"),
            (1, "short_term", "190000000"),
            (2, "peak_power", "230000000"),
        ] {
            m.set(&format!("{zone}/constraint_{i}_name"), name);
            m.set(&format!("{zone}/constraint_{i}_power_limit_uw"), uw);
        }
        m.set(&format!("{zone}/constraint_0_time_window_us"), "27983872");
        m.set(&format!("{zone}/constraint_1_time_window_us"), "2440");
        // as `legionctl record` keeps the live machine's read: the kernel answered ENODATA (61)
        m.set(&format!("{zone}/constraint_2_time_window_us.errno"), "61");
        let (own, other) = (root.join("proc/4329/fd"), root.join("proc/1/fd"));
        m.set("/proc/4329/comm", "systemd");
        m.set("/proc/1/comm", "systemd");
        for fd in [&own, &other] {
            std::fs::create_dir_all(fd).unwrap();
            std::os::unix::fs::symlink("/dev/nvidia0", fd.join("0")).unwrap();
        }
        // listed but not searchable: the links are denied; not readable: the folder is denied
        std::fs::set_permissions(&own, std::fs::Permissions::from_mode(0o444)).unwrap();
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o000)).unwrap();
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "notvisible");
        let json = |args: &[&str]| -> Value {
            serde_json::from_str(&op.ok(&[args, &["--json"]].concat())).unwrap()
        };
        let hidden = "2 processes not visible (another user's, or not dumpable)";

        let status = json(&["status"]);
        let rapl = &status["rapl"][0];
        assert_eq!(rapl["zone"], "intel-rapl-mmio:0", "{status}");
        assert_eq!(rapl["constraints"][2]["name"], "peak_power");
        assert_eq!(rapl["constraints"][2]["limit_w"], 230.0);
        assert!(rapl["constraints"][2]["window_us"].is_null(), "{rapl}");
        assert_eq!(rapl["constraints"][0]["window_us"], 27_983_872);
        let recorded = m.dir.join("recorded");
        op.ok(&["record", recorded.to_str().unwrap()]);
        assert_eq!(
            std::fs::read_to_string(
                recorded.join(format!("root{zone}/constraint_2_time_window_us.errno"))
            )
            .unwrap(),
            "61"
        );

        let gpu = op.ok(&["gpu", "status"]);
        assert!(
            gpu.contains("keeps it awake: gnome-shell (pid 4242) uses /dev/dri/card0")
                && gpu.contains(hidden),
            "{gpu}"
        );
        assert_eq!(json(&["gpu", "status"])["nvidia_power"]["not_visible"], 2);
        let resources = op.ok(&["resources", "list"]);
        assert!(resources.contains(hidden), "{resources}");
        let list = json(&["resources", "list"]);
        assert_eq!(list["not_visible"], 2, "{list}");
        let nvidia = list["resources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "gpu:nvidia")
            .unwrap_or_else(|| panic!("no gpu:nvidia in {list}"));
        assert_eq!(nvidia["users"], serde_json::json!([]), "{nvidia}");
        op.ok(&["knobs", "list"]);
        op.ok(&["plan"]);

        let out = op.cli(&["vfio", "bind", "0000:06:00.0"]);
        let text = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success()
                && text.contains("2 processes cannot be read: the host-use check cannot run"),
            "{text}"
        );
        assert_eq!(
            m.file("/sys/bus/pci/devices/0000:06:00.0/driver_override"),
            "(null)",
            "a refusal writes nothing"
        );
        for fd in [&own, &other] {
            std::fs::set_permissions(fd, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// An NTFS volume as far as its dirty flag needs: the boot sector (512-byte sectors, 4 KiB clusters, the MFT at
    /// cluster 4, 1 KiB records) and MFT record 3 (`$Volume`) with its update sequence and its `$VOLUME_INFORMATION`.
    fn ntfs_volume(dirty: bool) -> Vec<u8> {
        let mut v = vec![0u8; 16384 + 4 * 1024];
        v[3..11].copy_from_slice(b"NTFS    ");
        v[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
        v[0x0D] = 8;
        v[0x30..0x38].copy_from_slice(&4u64.to_le_bytes());
        v[0x40] = (-10i8).to_ne_bytes()[0];
        let r = &mut v[16384 + 3 * 1024..16384 + 4 * 1024];
        r[..4].copy_from_slice(b"FILE");
        r[4..6].copy_from_slice(&0x30u16.to_le_bytes());
        r[6..8].copy_from_slice(&3u16.to_le_bytes());
        r[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes());
        // the update sequence number, at the end of each 512 bytes (the array keeps the zeros that were there)
        r[0x30..0x32].copy_from_slice(&[7, 0]);
        r[510..512].copy_from_slice(&[7, 0]);
        r[1022..1024].copy_from_slice(&[7, 0]);
        // $VOLUME_INFORMATION: resident, its value at 0x18 (version 3.1, then the flags)
        r[0x38..0x3C].copy_from_slice(&0x70u32.to_le_bytes());
        r[0x3C..0x40].copy_from_slice(&0x28u32.to_le_bytes());
        r[0x38 + 0x14..0x38 + 0x16].copy_from_slice(&0x18u16.to_le_bytes());
        r[0x38 + 0x18 + 8] = 3;
        r[0x38 + 0x18 + 9] = 1;
        r[0x38 + 0x18 + 0x0A] = u8::from(dirty);
        r[0x60..0x64].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        v
    }

    /// The Windows disk's block device (4 MiB of known bytes), and `qemu-img` (overlay, commit, image) and `qemu-io`
    /// (the VM's writes) on the operator's and daemon's PATH; the stand-in QEMU writes its arguments and the disk's mode.
    fn windows_disk(m: &Machine) {
        let (root, bin) = (m.dir.join("root"), m.dir.join("bin"));
        // the whole disk: 4 MiB of known bytes (512-byte sectors)
        m.set("/sys/block/nvme0n1/size", "8192");
        std::fs::create_dir_all(root.join("dev")).unwrap();
        // its two NTFS partitions, clean
        for part in ["nvme0n1p3", "nvme0n1p4"] {
            std::fs::write(root.join("dev").join(part), ntfs_volume(false)).unwrap();
        }
        std::fs::write(
            root.join("dev/nvme0n1"),
            (0..4u32 << 20)
                .map(|i| u8::try_from(i % 251).unwrap())
                .collect::<Vec<u8>>(),
        )
        .unwrap();
        let qemu_img = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|d| d.join("qemu-img"))
            .find(|p| p.exists())
            .expect("qemu-img is installed (package qemu-img or qemu-utils)");
        std::os::unix::fs::symlink(qemu_img.with_file_name("qemu-io"), bin.join("qemu-io"))
            .unwrap();
        std::os::unix::fs::symlink(qemu_img, bin.join("qemu-img")).unwrap();
        Machine::script(
            &bin,
            "qemu-system-x86_64",
            // and the disk's mode while it runs (an overlay start makes it readable for the runner)
            &format!(
                "printf '%s\\n' \"$@\" > {dir}/qemu.args; stat -c '%a' {disk} > {dir}/qemu.disk 2>/dev/null; exit 0",
                dir = m.dir.display(),
                disk = root.join("dev/nvme0n1").display()
            ),
        );
    }

    /// The recorded machine with its Windows install (note 05): the SK hynix `NVMe` of `pci_machine` holds
    /// `nvme0n1p3` (system, read-only automount, Fast Startup off, not hibernated) and `nvme0n1p4` (data, read-write
    /// automount), the firmware has an `MSDM` table, the Bolt receiver is plugged in; `systemctl` logs its calls and
    /// `swtpm`, `taskset` and QEMU are stand-ins (QEMU writes its arguments). Returns the stand-in kernel's thread.
    pub(super) fn windows_machine(m: &Machine) -> std::thread::JoinHandle<()> {
        let kernel = pci_machine(m);
        let root = m.dir.join("root");
        let bin = m.dir.join("bin");
        let link = |from: &str, to: &str| {
            let at = root.join(from);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(to, at).unwrap();
        };
        m.set(
            "/etc/fstab",
            "UUID=1882270A8226EBC6 /mnt/windows-system ntfs3 ro,noauto,x-systemd.automount 0 0\n\
             UUID=46648D34648D282F /mnt/windows-data ntfs3 rw,noauto,x-systemd.automount 0 0\n\
             UUID=0d4f /home ext4 defaults 0 2",
        );
        link("dev/disk/by-uuid/1882270A8226EBC6", "../../nvme0n1p3");
        link("dev/disk/by-uuid/46648D34648D282F", "../../nvme0n1p4");
        link("dev/disk/by-uuid/0d4f", "../../nvme1n1p2");
        link(
            "sys/class/nvme/nvme0/device",
            "../../../devices/pci0000:00/0000:00:06.0/0000:06:00.0",
        );
        link(
            "sys/class/nvme/nvme1/device",
            "../../../devices/pci0000:00/0000:00:1d.0/0000:7c:00.0",
        );
        let hive = root.join("mnt/windows-system/Windows/System32/config/SYSTEM");
        std::fs::create_dir_all(hive.parent().unwrap()).unwrap();
        std::fs::write(&hive, HiveFile::system(1, 0)).unwrap();
        std::fs::write(root.join("mnt/windows-system/hiberfil.sys"), [0u8; 64]).unwrap();
        m.set("/sys/firmware/acpi/tables/MSDM", "MSDM-license");
        windows_disk(m);
        for (file, value) in [
            ("idVendor", "046d"),
            ("idProduct", "c548"),
            ("busnum", "1"),
            ("devnum", "5"),
        ] {
            m.set(&format!("/sys/bus/usb/devices/1-5/{file}"), value);
        }
        m.set("/dev/bus/usb/001/005", "");
        let log = m.dir.join("systemctl.log");
        Machine::script(
            &bin,
            "systemctl",
            // no unit runs in the test: is-active answers inactive (3), as systemd does
            // the VM's unit is active while the test makes the file unit-active
            // a start of units fails while the test makes the file start-fails
            &format!(
                "echo \"$*\" >> {}; case \"$*\" in *is-active*) [ -e {} ] && exit 0; exit 3;; \
                 start\\ *) [ -e {} ] && echo 'Unit not found.' >&2 && exit 5;; esac; exit 0",
                log.display(),
                m.dir.join("unit-active").display(),
                m.dir.join("start-fails").display()
            ),
        );
        Machine::script(
            &bin,
            "efibootmgr",
            "printf 'BootCurrent: 0003\\nBoot0004* Windows Boot Manager\\tHD(1,GPT,e8,0x800,0x82000)/\\\\EFI\\\\Microsoft\\\\Boot\\\\bootmgfw.efi\\n'",
        );
        Machine::script(
            &bin,
            "taskset",
            &format!(
                "echo \"$2\" > {}/taskset.cpus; shift 2; exec \"$@\"",
                m.dir.display()
            ),
        );
        Machine::script(
            &bin,
            "swtpm",
            "exec python3 -c 'import re,socket,sys,time; s=socket.socket(socket.AF_UNIX); \
             s.bind(re.search(r\"path=([^ ]+)\", \" \".join(sys.argv)).group(1)); s.listen(); time.sleep(60)' \"$@\"",
        );
        std::fs::create_dir_all(m.dir.join("share")).unwrap();
        Machine::script(
            &bin,
            "virtiofsd",
            "exec python3 -c 'import re,socket,sys,time; s=socket.socket(socket.AF_UNIX); \
             s.bind(re.search(r\"--socket-path=([^ ]+)\", \" \".join(sys.argv)).group(1)); s.listen(); time.sleep(60)' \"$@\"",
        );
        Machine::script(
            &bin,
            "loginctl",
            &format!("echo \"loginctl $*\" >> {}", log.display()),
        );
        let vars = m.dir.join("OVMF_VARS.fd");
        std::fs::write(&vars, "vars").unwrap();
        m.set("/home/.config/legionctl/windows.jsonc", "");
        let config = m.dir.join("home/.config/legionctl/windows.jsonc");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            &config,
            serde_json::json!({
                "windows": {"vm": {"memory_mib": 64, "reserve_mib": 0, "ovmf_vars": vars, "shares": [m.dir.join("share")]}},
                "session": {"vm": {"virtiofsd": bin.join("virtiofsd")}}
            })
            .to_string(),
        )
        .unwrap();
        kernel
    }

    /// The Windows VM's runner as the operator's user: a wrapper writes its own pid's status into the recorded `/proc`
    /// (the daemon checks whose process may lock memory) and becomes `legionctl windows-vm`.
    fn windows_runner(m: &Machine, op: &Operator) -> std::process::Output {
        let uid = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata("/proc/self").unwrap());
        let legionctl = Path::new(env!("CARGO_BIN_EXE_legionctl"));
        let wrapper = format!(
            "mkdir -p {root}/proc/$$ && printf 'Name:\\tlegionctl\\nUid:\\t{uid}\\t{uid}\\t{uid}\\t{uid}\\n' > {root}/proc/$$/status \
             && exec {legionctl} windows-vm",
            root = m.dir.join("root").display(),
            legionctl = legionctl.display()
        );
        let path = std::env::join_paths([m.dir.join("bin"), "/usr/bin".into()]).unwrap();
        std::process::Command::new("sh")
            .args(["-c", &wrapper])
            .env_clear()
            .env("HOME", m.dir.join("home"))
            .env("XDG_RUNTIME_DIR", &op.runtime)
            .env("PATH", path)
            .output()
            .unwrap()
    }

    fn windows_state(m: &Machine) -> String {
        std::fs::read_to_string(m.dir.join("home/.local/share/legionctl/windows/state"))
            .unwrap_or_default()
    }

    /// A Windows VM start needs proof that Windows shut down fully (B1): a hibernated Windows, a hibernation file that
    /// cannot be read or has an unknown header, a registry that cannot be read (Fast Startup unknown), and a volume of
    /// the Windows disk whose own flag says dirty or that cannot be read are each refused before anything changes, with
    /// what to do; an old dirty line in the kernel log does not refuse.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_windows_vm_start_needs_proof_that_windows_shut_down_fully() {
        let m = Machine::new("winsafe");
        let _kernel = windows_machine(&m);
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "winsafe");
        let systemctl = || std::fs::read_to_string(m.dir.join("systemctl.log")).unwrap_or_default();
        // each state that does not prove a full shut down: refused before anything changes
        let hiberfil = m.dir.join("root/mnt/windows-system/hiberfil.sys");
        let hive = m
            .dir
            .join("root/mnt/windows-system/Windows/System32/config/SYSTEM");
        let data = m.dir.join("root/dev/nvme0n1p4");
        let refused = |why: &str| {
            assert!(!windows_runner(&m, &op).status.success());
            let state = windows_state(&m);
            assert!(state.contains(why), "{why} missing: {state}");
            assert!(
                state.contains("start Windows on the machine and shut it down fully")
                    || state.contains("Start Windows and shut it down with Fast Startup off"),
                "says what to do: {state}"
            );
            assert!(
                !systemctl().contains("automount"),
                "nothing changed: {}",
                systemctl()
            );
        };
        std::fs::write(&hiberfil, b"hibr and a resume image").unwrap();
        refused("Windows is hibernated");
        std::fs::write(&hiberfil, b"RSTRa resume in progress").unwrap();
        refused("starts with an unknown header (RSTR)");
        std::fs::write(&hiberfil, [0u8; 64]).unwrap();
        std::fs::set_permissions(
            &hiberfil,
            std::os::unix::fs::PermissionsExt::from_mode(0o000),
        )
        .unwrap();
        refused("hiberfil.sys cannot be read");
        std::fs::set_permissions(
            &hiberfil,
            std::os::unix::fs::PermissionsExt::from_mode(0o644),
        )
        .unwrap();
        std::fs::write(&hive, b"regf but cut short").unwrap();
        refused("the Windows registry cannot be read");
        std::fs::write(&hive, HiveFile::system(1, 0)).unwrap();
        // the flag on the volume itself (a dirty line of the kernel log from before a VM run that cleaned it is old)
        std::fs::write(&data, ntfs_volume(true)).unwrap();
        refused("NTFS volume nvme0n1p4 is marked dirty");
        std::fs::write(&data, b"not a file system").unwrap();
        refused("the NTFS volume nvme0n1p4 cannot be read");
        std::fs::write(&data, ntfs_volume(false)).unwrap();
        std::fs::write(
            m.dir.join("kernel.log"),
            "ntfs3(nvme0n1p4): volume is dirty and \"force\" flag is not set!\n",
        )
        .unwrap();

        // proof again: the start goes on to the next guard
        assert!(!windows_runner(&m, &op).status.success());
        assert!(
            windows_state(&m).contains("/dev/nvme0n1p3 is mounted at /mnt/windows-system"),
            "{}",
            windows_state(&m)
        );
    }

    /// The Windows VM's guards and their undo (note 05): a mounted Windows partition is refused before anything is
    /// bound, and the stopped automounts start again; then a start that fails half way (here: the memory lock, which
    /// only root may raise this far) gives the disk back to its driver, starts the automounts again and leaves no
    /// record, and the runner's state says why; QEMU never runs. An undo step that fails keeps its record: the next
    /// stop tries only that step again, and the record goes when it works.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_windows_vm_start_is_refused_or_undone_without_leaving_the_host_changed() {
        let m = Machine::new("winvm");
        let _kernel = windows_machine(&m);
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "winvm");
        let driver = || {
            std::fs::read_link(m.dir.join("root/sys/bus/pci/devices/0000:06:00.0/driver")).unwrap()
        };
        let systemctl = || std::fs::read_to_string(m.dir.join("systemctl.log")).unwrap_or_default();

        let out = windows_runner(&m, &op);
        assert!(!out.status.success());
        let state = windows_state(&m);
        assert!(
            state.contains("failed: ")
                && state.contains("/dev/nvme0n1p3 is mounted at /mnt/windows-system"),
            "{state}"
        );
        assert_eq!(driver(), Path::new("../../drivers/nvme"), "nothing bound");
        let log = systemctl();
        for line in [
            "stop mnt-windows\\x2dsystem.automount mnt-windows\\x2dsystem.mount",
            "stop mnt-windows\\x2ddata.automount mnt-windows\\x2ddata.mount",
            "start mnt-windows\\x2dsystem.automount mnt-windows\\x2ddata.automount",
        ] {
            assert!(log.contains(line), "{line} missing:\n{log}");
        }
        assert!(
            !log.contains("home"),
            "only the Windows disk's automounts: {log}"
        );

        m.set("/proc/1/mounts", "/dev/nvme1n1p2 / btrfs rw 0 0");
        std::fs::remove_file(m.dir.join("systemctl.log")).unwrap();
        let out = windows_runner(&m, &op);
        assert!(!out.status.success());
        let state = windows_state(&m);
        assert!(
            state.contains("setting the memory lock limit")
                && state.contains("0000:06:00.0: given back"),
            "{state}"
        );
        assert_eq!(
            driver(),
            Path::new("../../drivers/nvme"),
            "the disk is back with its driver"
        );
        assert!(
            systemctl()
                .contains("start mnt-windows\\x2dsystem.automount mnt-windows\\x2ddata.automount")
        );
        assert!(!m.dir.join("qemu.args").exists(), "QEMU never ran");
        assert!(
            op.ok(&["windows", "vm", "stop"])
                .contains("no Windows VM ran"),
            "no record is left"
        );

        // an undo step that fails: its record stays, and the next stop tries it again
        std::fs::write(m.dir.join("start-fails"), "").unwrap();
        assert!(!windows_runner(&m, &op).status.success());
        let state = windows_state(&m);
        assert!(
            state.contains("0000:06:00.0: given back")
                && state.contains("warning: started mnt-windows\\x2dsystem.automount")
                && state.contains("run legionctl windows vm stop again"),
            "{state}"
        );
        let again = op.cli(&["windows", "vm", "stop"]);
        let text = String::from_utf8_lossy(&again.stderr);
        assert!(
            !again.status.success()
                && text.contains("not undone")
                && !text.contains("0000:06:00.0"),
            "only the step that failed runs again: {text}"
        );
        std::fs::remove_file(m.dir.join("start-fails")).unwrap();
        assert!(
            op.ok(&["windows", "vm", "stop"]).contains(
                "started mnt-windows\\x2dsystem.automount mnt-windows\\x2ddata.automount"
            )
        );
        assert!(
            op.ok(&["windows", "vm", "stop"])
                .contains("no Windows VM ran"),
            "the record goes when each step is undone"
        );
    }

    /// Autostart (note 05): `login` and `boot` let this user, and only for the Windows VM action, start and end the VM
    /// without a prompt (a polkit rule, after one approval) and enable its unit; `boot` also starts the user's services
    /// at boot; `off` takes the rule away. `view` needs a running VM.
    #[tokio::test(flavor = "multi_thread")]
    async fn windows_vm_autostart_is_a_rule_for_this_user_and_this_action_only() {
        let m = Machine::new("winauto");
        let _kernel = windows_machine(&m);
        let uid = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata("/proc/self").unwrap());
        m.set(
            "/etc/passwd",
            &format!("root:x:0:0::/root:/bin/sh\ntester:x:{uid}:{uid}::/home/tester:/bin/sh"),
        );
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "winauto");
        let rule = m
            .dir
            .join("root/etc/polkit-1/rules.d/49-legionctl-windows-vm-tester.rules");
        let log = || std::fs::read_to_string(m.dir.join("systemctl.log")).unwrap_or_default();

        op.ok(&["windows", "vm", "autostart", "login"]);
        let text = std::fs::read_to_string(&rule).unwrap();
        assert!(
            text.contains(
                "action.id == \"org.legionctl.windows.vm\" && subject.user == \"tester\" && subject.local && subject.active"
            ) && text.matches("polkit.Result.YES").count() == 1,
            "{text}"
        );
        assert!(
            log().contains("--user enable legionctl-windows-vm.service"),
            "{}",
            log()
        );
        assert!(!log().contains("enable-linger"), "login: no lingering");

        op.ok(&["windows", "vm", "autostart", "boot"]);
        assert!(log().contains("loginctl enable-linger"), "{}", log());

        op.ok(&["windows", "vm", "autostart", "off"]);
        assert!(!rule.exists(), "off asks again");
        assert!(
            log().contains("--user disable legionctl-windows-vm.service"),
            "{}",
            log()
        );

        let view = op.cli(&["windows", "vm", "view"]);
        assert!(
            String::from_utf8_lossy(&view.stderr).contains("the Windows VM does not run"),
            "{}",
            String::from_utf8_lossy(&view.stderr)
        );
    }

    /// The Windows disk's bytes (the recorded block device).
    fn disk(m: &Machine) -> Vec<u8> {
        std::fs::read(m.dir.join("root/dev/nvme0n1")).unwrap()
    }

    /// What the VM writes on the overlay: `qemu-io` writes 64 KiB of 0xab at 1 MiB through the overlay, as QEMU
    /// opens it (the backing named, not taken from the header).
    fn vm_writes(m: &Machine) {
        let out = std::process::Command::new(m.dir.join("bin/qemu-io"))
            .arg("--image-opts")
            .arg(format!(
                "driver=qcow2,file.filename={},backing.driver=raw,backing.file.filename={}",
                m.dir
                    .join("home/.local/share/legionctl/windows/overlay.qcow2")
                    .display(),
                m.dir.join("root/dev/nvme0n1").display()
            ))
            .args(["-c", "write -P 0xab 1M 64k"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Backup before repair (todo-151 a, W1/S1): a VM start with an overlay never binds the disk to VFIO and only
    /// reads it (the runner may read it while QEMU runs; owner and mode come back after), and QEMU gets the overlay
    /// with the disk as its backing. A hibernated Windows is refused by the disk guard. Drop leaves every byte of the
    /// disk as it was. An overlay is not made while the VM runs.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_windows_repair_runs_on_an_overlay_that_is_committed_or_dropped() {
        let m = Machine::new("winoverlay");
        let _kernel = windows_machine(&m);
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "winoverlay");
        let before = disk(&m);
        let node = m.dir.join("root/dev/nvme0n1");
        let mode = || {
            std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(&node).unwrap().permissions(),
            ) & 0o7777
        };
        let mode_before = mode();
        let err = |args: &[&str]| {
            let out = op.cli(args);
            assert!(!out.status.success(), "{args:?} passed");
            String::from_utf8_lossy(&out.stderr).to_string()
        };

        std::fs::write(m.dir.join("unit-active"), "").unwrap();
        assert!(err(&["windows", "vm", "overlay", "create"]).contains("the Windows VM runs"));
        std::fs::remove_file(m.dir.join("unit-active")).unwrap();
        let created = op.ok(&["windows", "vm", "overlay", "create"]);
        assert!(created.contains("over /dev/nvme0n1"), "{created}");

        let hiberfil = m.dir.join("root/mnt/windows-system/hiberfil.sys");
        std::fs::write(&hiberfil, b"hibr and a resume image").unwrap();
        assert!(!windows_runner(&m, &op).status.success());
        assert!(
            windows_state(&m).contains("Windows is hibernated"),
            "{}",
            windows_state(&m)
        );
        assert_eq!(mode(), mode_before, "a refusal changes nothing");
        std::fs::write(&hiberfil, [0u8; 64]).unwrap();
        // the guard: a mounted Windows partition (pci_machine's mount table)
        assert!(!windows_runner(&m, &op).status.success());
        assert!(
            windows_state(&m).contains("/dev/nvme0n1p3 is mounted at /mnt/windows-system"),
            "{}",
            windows_state(&m)
        );
        assert_eq!(mode(), mode_before, "a refusal changes nothing");

        m.set("/proc/1/mounts", "/dev/nvme1n1p2 / btrfs rw 0 0");
        let out = windows_runner(&m, &op);
        assert!(out.status.success(), "{}", windows_state(&m));
        let args = std::fs::read_to_string(m.dir.join("qemu.args")).unwrap();
        assert!(
            args.contains(&format!(
                "if=none,id=disk,driver=qcow2,file.filename={},backing.driver=raw,backing.file.filename={}",
                m.dir.join("home/.local/share/legionctl/windows/overlay.qcow2").display(),
                node.display()
            )) && args.contains("nvme,drive=disk"),
            "{args}"
        );
        assert!(
            !args.contains("vfio-pci,host=0000:06:00.0"),
            "no VFIO: {args}"
        );
        assert_eq!(
            std::fs::read_link(m.dir.join("root/sys/bus/pci/devices/0000:06:00.0/driver")).unwrap(),
            Path::new("../../drivers/nvme"),
            "the disk stays on its driver"
        );
        assert_eq!(
            std::fs::read_to_string(m.dir.join("qemu.disk"))
                .unwrap()
                .trim(),
            "460",
            "QEMU may only read the disk"
        );
        assert_eq!(mode(), mode_before, "owner and mode back after the VM");

        vm_writes(&m);
        assert_eq!(disk(&m), before, "the VM's writes stay in the overlay");
        op.ok(&["windows", "vm", "overlay", "drop"]);
        assert_eq!(disk(&m), before, "drop: the disk as it was");
        assert!(
            op.ok(&["windows", "vm", "overlay", "status"])
                .contains("no overlay")
        );
    }

    /// Commit (todo-151 a, W1/S1): after Windows ran on the disk (its SYSTEM hive changed) a commit is refused and
    /// writes nothing; else it writes exactly the VM's writes into the disk, with the automounts stopped around it,
    /// and the overlay goes.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_overlay_commit_writes_the_vms_writes_only_when_windows_did_not_run_since() {
        let m = Machine::new("wincommit");
        let _kernel = windows_machine(&m);
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "wincommit");
        m.set("/proc/1/mounts", "/dev/nvme1n1p2 / btrfs rw 0 0");
        let before = disk(&m);
        let systemctl = || std::fs::read_to_string(m.dir.join("systemctl.log")).unwrap_or_default();
        op.ok(&["windows", "vm", "overlay", "create"]);
        vm_writes(&m);
        let hive = m
            .dir
            .join("root/mnt/windows-system/Windows/System32/config/SYSTEM");
        let hive_bytes = std::fs::read(&hive).unwrap();
        std::fs::write(&hive, [hive_bytes.as_slice(), b"bare metal ran"].concat()).unwrap();
        let refused = op.cli(&["windows", "vm", "overlay", "commit"]);
        assert!(
            String::from_utf8_lossy(&refused.stderr)
                .contains("Windows ran on its disk since the overlay was made")
        );
        assert_eq!(disk(&m), before, "a refused commit writes nothing");
        std::fs::write(&hive, &hive_bytes).unwrap();
        // bare metal hibernated with the hive as it was: the disk guard still refuses
        let hiberfil = m.dir.join("root/mnt/windows-system/hiberfil.sys");
        std::fs::write(&hiberfil, b"hibr and a resume image").unwrap();
        let refused = op.cli(&["windows", "vm", "overlay", "commit"]);
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("Windows is hibernated"),
            "{}",
            String::from_utf8_lossy(&refused.stderr)
        );
        assert_eq!(disk(&m), before, "a refused commit writes nothing");
        std::fs::write(&hiberfil, [0u8; 64]).unwrap();

        std::fs::remove_file(m.dir.join("systemctl.log")).unwrap();
        op.ok(&["windows", "vm", "overlay", "commit"]);
        let after = disk(&m);
        let (at, len) = (1 << 20, 64 << 10);
        assert!(
            after[at..at + len].iter().all(|b| *b == 0xab),
            "the VM's write reached the disk"
        );
        assert!(
            after[..at] == before[..at] && after[at + len..] == before[at + len..],
            "nothing else changed"
        );
        let log = systemctl();
        assert!(
            log.contains("stop mnt-windows\\x2dsystem.automount mnt-windows\\x2dsystem.mount")
                && log.contains(
                    "start mnt-windows\\x2dsystem.automount mnt-windows\\x2ddata.automount"
                ),
            "{log}"
        );
        assert!(
            !m.dir
                .join("home/.local/share/legionctl/windows/overlay.qcow2")
                .exists()
        );
    }

    /// A full image on request (todo-151 a): a new qcow2 file that holds every byte of the disk, with its hash; the
    /// disk is only read for it and its owner and mode come back; a file that exists is refused.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_full_image_of_the_windows_disk_holds_its_bytes_and_its_hash() {
        let m = Machine::new("winimage");
        let _kernel = windows_machine(&m);
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "winimage");
        m.set("/proc/1/mounts", "/dev/nvme1n1p2 / btrfs rw 0 0");
        let node = m.dir.join("root/dev/nvme0n1");
        let mode = || std::fs::metadata(&node).unwrap().permissions();
        let mode_before = mode();
        let image = m.dir.join("windows.qcow2");
        let line = op.ok(&["windows", "vm", "image", image.to_str().unwrap()]);
        assert!(line.contains("blake2b-512 "), "{line}");
        let raw = m.dir.join("image.raw");
        let out = std::process::Command::new(m.dir.join("bin/qemu-img"))
            .args(["convert", "-O", "raw"])
            .arg(&image)
            .arg(&raw)
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(
            std::fs::read(&raw).unwrap(),
            disk(&m),
            "the image holds the disk"
        );
        assert_eq!(mode(), mode_before, "the image gives the disk back");
        let again = op.cli(&["windows", "vm", "image", image.to_str().unwrap()]);
        assert!(String::from_utf8_lossy(&again.stderr).contains("exists"));
    }

    /// A stand-in QEMU guest agent of Windows at the VM's socket (todo-151): `guest-sync`, `guest-exec` (each
    /// program's command line goes to `qga.log`; its output goes to the log file its command line names, in Windows'
    /// encodings), `guest-exec-status` and the file reads. sfc writes UTF-16 in two parts 2.5 s apart; chkdsk ends
    /// with 3 (problems not fixed); reagentc fails while the file `winre-disabled` exists.
    fn qga_stand_in(m: &Machine) -> Kill {
        let dir = m.dir.join("home/.local/share/legionctl/windows");
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("qga.sock");
        let _ = std::fs::remove_file(&socket);
        let script = r#"
import base64, json, os, re, socket, sys, threading, time
path, log, guest = sys.argv[1], sys.argv[2], sys.argv[3]
os.makedirs(guest, exist_ok=True)
server = socket.socket(socket.AF_UNIX); server.bind(path); server.listen()
procs, handles = {}, {}
def run(pid, line):
    m = re.search(r"legionctl-\d+\.log", line)
    out = os.path.join(guest, m.group(0)) if m else None
    def write(data):
        if out:
            open(out, "ab").write(data)
    code = 0
    if "sfc /scannow" in line:
        write("Beginning system scan.  This process will take some time.\r\n".encode("utf-16-le"))
        time.sleep(2.5)
        write("Windows Resource Protection found corrupt files and successfully repaired them.\r\n".encode("utf-16-le"))
    elif "chkdsk" in line:
        write(b"Stage 1: Examining basic file system structure ...\r\nWindows found problems with the file system.\r\n"); code = 3
    elif "DISM" in line:
        write(b"The restore operation completed successfully.\r\n")
    elif "reagentc" in line:
        if os.path.exists(os.path.join(guest, "../winre-disabled")):
            write(b"REAGENTC.EXE: Windows RE is disabled.\r\n"); code = 2
        else:
            write(b"REAGENTC.EXE: Operation Successful.\r\n")
    elif "Get-ComputerRestorePoint" in line:
        write("\ufeff12|2026-10-01 10:00|Windows Update\r\n13|2026-10-12 09:30|Installed a driver\r\n".encode())
    procs[pid] = code
while True:
    conn, _ = server.accept()
    f = conn.makefile("rw")
    for raw in f:
        msg = json.loads(raw); cmd, a = msg["execute"], msg.get("arguments", {})
        if cmd == "guest-sync":
            reply = {"return": a["id"]}
        elif cmd == "guest-exec":
            pid = 100 + len(procs) + len(handles)
            procs[pid] = None
            line = " ".join([a["path"]] + a.get("arg", []))
            open(log, "a").write(line + "\n")
            threading.Thread(target=run, args=(pid, line), daemon=True).start()
            reply = {"return": {"pid": pid}}
        elif cmd == "guest-exec-status":
            code = procs.get(a["pid"])
            reply = {"return": {"exited": code is not None, "exitcode": code if code is not None else 0}}
        elif cmd == "guest-file-open":
            local = os.path.join(guest, a["path"].split("\\")[-1])
            if os.path.exists(local):
                handles[len(handles) + 1] = open(local, "rb").read()
                reply = {"return": len(handles)}
            else:
                reply = {"error": {"desc": "no such file"}}
        elif cmd == "guest-file-read":
            reply = {"return": {"count": len(handles[a["handle"]]), "buf-b64": base64.b64encode(handles[a["handle"]]).decode(), "eof": True}}
        else:
            reply = {"return": {}}
        f.write(json.dumps(reply) + "\n"); f.flush()
"#;
        let child = Kill::spawn(
            std::process::Command::new("python3")
                .args(["-c", script])
                .arg(&socket)
                .arg(m.dir.join("qga.log"))
                .arg(m.dir.join("guest")),
        );
        wait_for("the stand-in guest agent", || socket.exists());
        child
    }

    /// Repairs and restore points through the guest agent (todo-151 c and e, W3/S3): without an overlay nothing runs;
    /// with no agent answering the step fails and nothing runs; sfc's UTF-16 output comes in parts as events while
    /// it runs and "found and repaired" is shown as repaired; chkdsk's exit 3 fails with its reason; restore points
    /// are listed, an unknown one is refused before anything runs, and a known one is restored.
    #[tokio::test(flavor = "multi_thread")]
    async fn windows_repairs_and_restore_points_run_through_the_guest_agent_with_their_output() {
        let m = Machine::new("winrepair");
        let _kernel = windows_machine(&m);
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "winrepair");
        let err = |args: &[&str]| {
            let out = op.cli(args);
            assert!(!out.status.success(), "{args:?} passed");
            String::from_utf8_lossy(&out.stderr).to_string()
        };
        let ran = || std::fs::read_to_string(m.dir.join("qga.log")).unwrap_or_default();

        std::fs::write(m.dir.join("unit-active"), "").unwrap();
        assert!(err(&["windows", "repair", "sfc"]).contains("a repair runs only on an overlay"));
        std::fs::remove_file(m.dir.join("unit-active")).unwrap();
        op.ok(&["windows", "vm", "overlay", "create"]);
        std::fs::write(m.dir.join("unit-active"), "").unwrap();
        assert!(err(&["windows", "repair", "sfc"]).contains("guest agent socket"));

        let _agent = qga_stand_in(&m);
        let sfc = op.ok(&["windows", "repair", "sfc"]);
        assert!(
            sfc.contains("sfc /scannow: sfc: found corrupt files and repaired them"),
            "{sfc}"
        );
        let events: Value = serde_json::from_str(&op.ok(&["events", "--json"])).unwrap();
        let parts: Vec<String> = events["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "windows.repair.output")
            .map(|e| e["payload"]["text"].as_str().unwrap().to_string())
            .collect();
        assert!(
            parts.len() >= 2
                && parts[0].contains("Beginning system scan")
                && !parts[0].contains("repaired"),
            "the output came while sfc ran: {parts:?}"
        );
        assert!(
            err(&["windows", "repair", "chkdsk"])
                .contains("chkdsk found problems it cannot fix while Windows runs (exit 3)")
        );

        assert_eq!(
            op.ok(&["windows", "restore", "list"]),
            "12  2026-10-01 10:00  Windows Update\n13  2026-10-12 09:30  Installed a driver\n"
        );
        assert!(err(&["windows", "restore", "apply", "99"]).contains("no restore point 99"));
        assert!(!ran().contains("Restore-Computer"), "{}", ran());
        op.ok(&["windows", "restore", "apply", "12"]);
        assert!(
            ran().contains("Restore-Computer -RestorePoint 12 -Confirm:$false"),
            "{}",
            ran()
        );
    }

    /// Windows RE and boot repair (todo-151 b and d, W2/S2): with Windows RE disabled the recovery start fails with Windows'
    /// reason and Windows does not restart; else Windows RE is set for the next start and Windows restarts. Boot repair is
    /// refused while Windows (its guest agent) answers, so no key reaches Windows' desktop; in Windows RE it types the
    /// bcdboot command and Enter into the console.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_windows_vm_starts_into_winre_and_runs_bcdboot_there() {
        let m = Machine::new("winre");
        let _kernel = windows_machine(&m);
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "winre");
        let err = |args: &[&str]| {
            let out = op.cli(args);
            assert!(!out.status.success(), "{args:?} passed");
            String::from_utf8_lossy(&out.stderr).to_string()
        };
        let ran = || std::fs::read_to_string(m.dir.join("qga.log")).unwrap_or_default();
        op.ok(&["windows", "vm", "overlay", "create"]);
        std::fs::write(m.dir.join("unit-active"), "").unwrap();
        let agent = qga_stand_in(&m);
        let _qmp = qmp_stand_in(&m);

        std::fs::write(m.dir.join("winre-disabled"), "").unwrap();
        assert!(err(&["windows", "vm", "recovery"]).contains("Windows RE is disabled"));
        assert!(!ran().contains("shutdown.exe"), "{}", ran());
        std::fs::remove_file(m.dir.join("winre-disabled")).unwrap();
        assert!(
            op.ok(&["windows", "vm", "recovery"])
                .contains("restarts into WinRE")
        );
        assert!(ran().ends_with("shutdown.exe /r /t 5\n"), "{}", ran());

        assert!(err(&["windows", "repair", "boot"]).contains("Windows runs, not WinRE"));
        let keys = || std::fs::read_to_string(m.dir.join("qmp.log")).unwrap_or_default();
        assert_eq!(keys(), "", "no key reached Windows");
        drop(agent);
        op.ok(&["windows", "repair", "boot"]);
        let typed: String = keys()
            .lines()
            .map(|l| {
                let keys: Value =
                    serde_json::from_str(l.strip_prefix("send-key ").unwrap()).unwrap();
                let codes: Vec<&str> = keys["keys"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|k| k["data"].as_str().unwrap())
                    .collect();
                let (shift, code) = match codes.as_slice() {
                    ["shift", code] => (true, *code),
                    [code] => (false, *code),
                    other => panic!("{other:?}"),
                };
                match (shift, code) {
                    (false, "spc") => ' ',
                    (false, "ret") => '\n',
                    (false, "backslash") => '\\',
                    (false, "slash") => '/',
                    (false, "dot") => '.',
                    (true, "semicolon") => ':',
                    (true, "5") => '%',
                    (true, "2") => '@',
                    (true, "9") => '(',
                    (true, "0") => ')',
                    (true, c) => c.to_ascii_uppercase().chars().next().unwrap(),
                    (false, c) => c.chars().next().unwrap(),
                }
            })
            .collect();
        assert_eq!(
            typed,
            "for %d in (C D E F G H I J K L M N O P Q R S T U V W Y Z) do @if exist %d:\\Windows\\System32\\config\\SYSTEM bcdboot %d:\\Windows /f UEFI\n"
        );
    }

    /// Host actions from inside Windows (todo-151 f, W4/S4): a stand-in QEMU asks on the VM's host channel as Windows
    /// does. Denied on the Linux desktop: nothing happens and Windows is told; an unknown request is refused without a
    /// question; the restart into Windows bare metal is refused while an overlay holds changes; each question says
    /// "whole machine"; allowed, the restart into Linux and the restart into Windows run with the host's approval
    /// (polkit for the latter, `BootNext` first).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_host_action_from_inside_windows_needs_the_hosts_approval() {
        let m = Machine::new("winhost");
        let _kernel = windows_machine(&m);
        let (_stand_ins, asked_polkit, _processes) = start(&m).await;
        let address = format!("unix:path={}", m.dir.join("bus").display());
        let (_desktop, asked) =
            crate::desktop::answering(&address, &["deny", "allow", "allow", "allow", "allow"])
                .await;
        let op = Operator::start(
            "winhost",
            &[
                ("PATH", m.dir.join("bin").into()),
                ("HOME", m.dir.join("home").into()),
                ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
                ("DBUS_SYSTEM_BUS_ADDRESS", address.clone().into()),
                ("DBUS_SESSION_BUS_ADDRESS", address.into()),
            ],
        );
        let log = m.dir.join("systemctl.log");
        Machine::script(
            &m.dir.join("bin"),
            "efibootmgr",
            &format!(
                "if [ $# -eq 0 ]; then printf 'BootCurrent: 0003\\nBoot0004* Windows Boot Manager\\tHD(1,GPT,e8,0x800,0x82000)/\\\\EFI\\\\Microsoft\\\\Boot\\\\bootmgfw.efi\\n'; exit 0; fi; echo \"efibootmgr $*\" >> {}",
                log.display()
            ),
        );
        // Windows: the requests one after another, each reply into replies.txt; the overlay goes before the last
        Machine::script(
            &m.dir.join("bin"),
            "qemu-system-x86_64",
            &format!(
                "exec python3 -c 'import re,socket,sys,os; p=re.search(r\"id=host,path=([^,]+)\", \" \".join(sys.argv)).group(1); \
                 s=socket.socket(socket.AF_UNIX); s.connect(p); f=s.makefile(\"rw\"); out=open(\"{replies}\",\"w\")\n\
                 for r in [\"shutdown\", \"format-c\", \"restart-windows\", \"restart\", \"-\", \"restart-windows\"]:\n\
                 \x20   if r == \"-\": os.remove(\"{overlay}\"); continue\n\
                 \x20   f.write(r + \"\\n\"); f.flush(); out.write(r + \" \" + f.readline())\n' \"$@\"",
                replies = m.dir.join("replies.txt").display(),
                overlay = m
                    .dir
                    .join("home/.local/share/legionctl/windows/overlay.qcow2")
                    .display()
            ),
        );
        op.ok(&["windows", "vm", "overlay", "create"]);
        m.set("/proc/1/mounts", "/dev/nvme1n1p2 / btrfs rw 0 0");
        std::fs::remove_file(&log).unwrap();
        let out = windows_runner(&m, &op);
        assert!(out.status.success(), "{}", windows_state(&m));

        let replies = std::fs::read_to_string(m.dir.join("replies.txt")).unwrap();
        let lines: Vec<&str> = replies.lines().collect();
        assert!(
            lines[0].starts_with("shutdown refused: ") && lines[0].contains("did not allow"),
            "{replies}"
        );
        assert!(
            lines[1].starts_with("format-c refused: format-c is not a host action"),
            "{replies}"
        );
        assert!(
            lines[2].starts_with("restart-windows refused: ")
                && lines[2].contains("an overlay holds repair changes"),
            "{replies}"
        );
        assert!(lines[3].starts_with("restart ok: "), "{replies}");
        assert!(lines[4].starts_with("restart-windows ok: "), "{replies}");
        let questions = asked.lock().unwrap().clone();
        assert_eq!(
            questions.len(),
            3,
            "a refusal before the question asks nothing: {questions:?}"
        );
        assert!(
            questions.iter().all(|q| q.contains("whole machine")),
            "{questions:?}"
        );
        assert!(
            questions[0].contains("shut down")
                && questions[1].contains("into Linux")
                && questions[2].contains("bare metal")
        );
        let commands: Vec<String> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .filter(|l| l.contains("poweroff") || l.contains("reboot") || l.contains("bootnext"))
            .map(String::from)
            .collect();
        assert_eq!(
            commands,
            ["reboot", "efibootmgr --bootnext 0004", "reboot"],
            "nothing for the denied shut down"
        );
        assert!(
            asked_polkit
                .lock()
                .unwrap()
                .iter()
                .any(|(_, a, _, _)| a == "org.legionctl.windows.reboot"),
            "polkit was asked for the restart into Windows"
        );
    }

    /// A stand-in QEMU QMP server at the Windows VM's socket: it answers the handshake, lists the devices that were
    /// added, logs every `device_add`/`device_del`/`stop`/`cont` to `qmp.log`, sends `DEVICE_DELETED` after a
    /// `device_del`, and answers `query-status` with running or paused (after `stop`, until `cont`).
    pub(super) fn qmp_stand_in(m: &Machine) -> Kill {
        let dir = m.dir.join("home/.local/share/legionctl/windows");
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("qmp.sock");
        let script = r#"
import json, os, socket, sys
path, log = sys.argv[1], sys.argv[2]
server = socket.socket(socket.AF_UNIX)
server.bind(path)
server.listen()
devices = []
status = "running"
while True:
    conn, _ = server.accept()
    f = conn.makefile("rw")
    f.write(json.dumps({"QMP": {"version": {}}}) + "\n"); f.flush()
    for line in f:
        msg = json.loads(line)
        cmd, args = msg["execute"], msg.get("arguments", {})
        if cmd in ("device_add", "device_del", "stop", "cont", "send-key"):
            open(log, "a").write(cmd + " " + json.dumps(args, sort_keys=True) + "\n")
        if cmd in ("stop", "cont"):
            status = "paused" if cmd == "stop" else "running"
        if cmd == "device_add":
            devices.append(args["id"])
        reply = {"return": [{"name": d} for d in devices] if cmd == "qom-list" else {"status": status} if cmd == "query-status" else {}}
        f.write(json.dumps(reply) + "\n"); f.flush()
        if cmd == "device_del":
            devices.clear()
            f.write(json.dumps({"event": "DEVICE_DELETED", "data": {"device": args["id"]}}) + "\n"); f.flush()
"#;
        let child = Kill::spawn(
            std::process::Command::new("python3")
                .args(["-c", script])
                .arg(&socket)
                .arg(m.dir.join("qmp.log")),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !socket.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the stand-in QMP did not start"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        child
    }

    /// GPU owners (note 05, `gpu give`): the NVIDIA GPU goes to Windows only when a VM runs (nothing moves before), is
    /// refused while Mutter holds it (with the way to release it, and its services started again), goes to the VM by
    /// hot-plug with function 0 last, and back to Linux by an eject request that Windows answers; `gpu primary intel
    /// --release-nvidia` makes Mutter leave it alone, which `nvidia` refuses.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_nvidia_gpu_moves_between_linux_and_the_windows_vm_by_its_owner_rules() {
        let m = Machine::new("gpugive");
        let _kernel = windows_machine(&m);
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "gpugive");
        let driver = || {
            std::fs::read_link(m.dir.join("root/sys/bus/pci/devices/0000:01:00.0/driver")).unwrap()
        };
        let systemctl = || std::fs::read_to_string(m.dir.join("systemctl.log")).unwrap_or_default();
        let refused = |args: &[&str], why: &str| {
            let out = op.cli(args);
            let text = String::from_utf8_lossy(&out.stderr).to_string();
            assert!(
                !out.status.success() && text.contains(why),
                "{why} missing:\n{text}"
            );
        };

        refused(&["gpu", "give", "nvidia", "windows"], "no Windows VM runs");
        assert_eq!(driver(), Path::new("../../drivers/nvidia"), "nothing moved");
        let _qmp = qmp_stand_in(&m);
        refused(&["gpu", "give", "nvidia", "windows"], "--release-nvidia");
        assert_eq!(driver(), Path::new("../../drivers/nvidia"));

        std::fs::remove_file(m.dir.join("root/proc/4242/fd/7")).unwrap();
        let given = op.ok(&["gpu", "give", "nvidia", "windows"]);
        assert!(given.contains("nvidia: linux -> windows"), "{given}");
        let qmp = std::fs::read_to_string(m.dir.join("qmp.log")).unwrap();
        let adds: Vec<&str> = qmp
            .lines()
            .filter(|l| l.starts_with("device_add"))
            .collect();
        assert!(
            adds.len() == 2
                && adds[0].contains("\"host\": \"0000:01:00.1\"")
                && adds[1].contains("\"host\": \"0000:01:00.0\"")
                && adds[1].contains("\"multifunction\": \"on\"")
                && adds.iter().all(|a| a.contains("\"bus\": \"gpu-port\"")),
            "function 0 last, into the free port:\n{qmp}"
        );
        assert_eq!(driver(), Path::new("../../drivers/vfio-pci"));
        assert!(
            op.ok(&["gpu", "give", "nvidia", "windows"])
                .contains("already windows")
        );

        let back = op.ok(&["gpu", "give", "nvidia", "linux"]);
        assert!(
            back.contains("Windows released it") && back.contains("windows -> linux"),
            "{back}"
        );
        assert!(
            std::fs::read_to_string(m.dir.join("qmp.log"))
                .unwrap()
                .contains("device_del {\"id\": \"gpu0\"}")
        );
        assert_eq!(driver(), Path::new("../../drivers/nvidia"));

        refused(
            &["gpu", "primary", "nvidia", "--release-nvidia"],
            "choose intel or auto",
        );
        let plan = op.ok(&["gpu", "primary", "intel", "--release-nvidia", "--dry-run"]);
        assert!(
            plan.contains("DRIVERS==\"nvidia\", TAG+=\"mutter-device-ignore\""),
            "{plan}"
        );
        let _ = systemctl;
    }

    /// The recorded machine's boot chain for `boot` (todo-80): an ESP at /boot/efi with shim and `MokManager` (rEFInd
    /// behind them), two kernels, the MOK key, systemd-boot; a stand-in `efibootmgr` that keeps the firmware's entries
    /// in a file (0003 rEFInd, 0004 Windows; this start 0003), and stand-in `ukify`/`sbsign` that write what they were
    /// given into their output.
    pub(super) fn boot_machine(m: &Machine) -> PathBuf {
        let bin = m.dir.join("bin");
        // the daemon's PATH in the test has only stand-ins; the boot plans use these system tools
        for tool in ["mkdir", "install", "rm", "mv", "bsdtar"] {
            std::os::unix::fs::symlink(format!("/usr/bin/{tool}"), bin.join(tool)).unwrap();
        }
        m.set(
            "/proc/self/mounts",
            "/dev/nvme1n1p2 / btrfs rw 0 0\n/dev/nvme1n1p1 /boot/efi vfat rw 0 0",
        );
        m.set(
            "/proc/cmdline",
            "quiet rw root=UUID=a9e6 initrd=\\initramfs-linux-cachyos-bore-lto.img",
        );
        for file in [
            "/boot/efi/EFI/BOOT/bootx64.efi",
            "/boot/efi/EFI/BOOT/mmx64.efi",
            "/boot/vmlinuz-linux-cachyos",
            "/boot/initramfs-linux-cachyos.img",
            "/boot/vmlinuz-linux-cachyos-bore-lto",
            "/boot/initramfs-linux-cachyos-bore-lto.img",
            "/boot/intel-ucode.img",
            "/etc/refind.d/keys/refind_local.key",
            "/etc/refind.d/keys/refind_local.crt",
            "/usr/lib/systemd/boot/efi/systemd-bootx64.efi",
        ] {
            m.set(file, "");
        }
        m.set(
            "/sys/devices/pci0000:00/0000:7c:00.0/nvme/nvme1/nvme1n1/nvme1n1p1/partition",
            "1",
        );
        let link = m.dir.join("root/sys/class/block/nvme1n1p1");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(
            "../../devices/pci0000:00/0000:7c:00.0/nvme/nvme1/nvme1n1/nvme1n1p1",
            link,
        )
        .unwrap();
        std::fs::create_dir_all(m.dir.join("root/sys/firmware/efi/efivars")).unwrap();
        let state = m.dir.join("efivars.json");
        std::fs::write(
            &state,
            r#"{"entries": {"0003": "Linpus lite", "0004": "Windows Boot Manager"}, "order": ["0003", "0004"], "current": "0003", "next": null}"#,
        )
        .unwrap();
        let efibootmgr = format!(
            r#"exec /usr/bin/python3 -c '
import json, sys
path = "{state}"
s = json.load(open(path)); a = sys.argv[1:]
open(path + ".log", "a").write(" ".join(a) + "\n")
if "--create-only" in a:
    s["entries"]["0007"] = a[a.index("--label") + 1]
elif "--bootnext" in a:
    s["next"] = a[a.index("--bootnext") + 1]
elif "--bootorder" in a:
    s["order"] = a[a.index("--bootorder") + 1].split(",")
elif "--delete-bootnum" in a:
    n = a[a.index("--bootnum") + 1]; s["entries"].pop(n); s["order"] = [o for o in s["order"] if o != n]
json.dump(s, open(path, "w"))
print("BootCurrent: " + s["current"])
if s["next"]: print("BootNext: " + s["next"])
print("BootOrder: " + ",".join(s["order"]))
for k, v in sorted(s["entries"].items()): print("Boot" + k + "* " + v + "\tHD(1,GPT,x)/" + ("\\EFI\\Microsoft\\Boot\\bootmgfw.efi" if v.startswith("Windows") else "File"))
' "$@""#,
            state = state.display()
        );
        Machine::script(&bin, "efibootmgr", &efibootmgr);
        let writer = |name: &str| {
            format!(
                "out=; prev=; for a in \"$@\"; do [ \"$prev\" = --output ] && out=$a; prev=$a; done; \
                 [ -z \"$out\" ] || echo \"{name} $*\" > \"$out\""
            )
        };
        Machine::script(&bin, "ukify", &writer("ukify"));
        Machine::script(&bin, "sbsign", &writer("sbsign"));
        // a signed image is one that ukify got the MOK key for
        Machine::script(
            &bin,
            "sbverify",
            "for a; do f=$a; done; /usr/bin/grep -q -- '--secureboot-private-key' \"$f\" || { echo 'No signature table present' >&2; exit 1; }",
        );
        state
    }

    /// The boot takeover (todo-80): legionctl's entry comes beside the old chain, never first and never alone: the
    /// next start tries it once (`BootNext`), `rebuild` follows the kernels, and `remove` leaves the old chain as it was. Each kernel's image is signed with the MOK key and has
    /// the running system's kernel line; systemd-boot is shim's signed second stage.
    #[tokio::test(flavor = "multi_thread")]
    async fn legionctl_boot_comes_beside_the_old_chain_and_is_tried_once_before_it_leads() {
        let m = Machine::new("boot");
        let state = boot_machine(&m);
        let (_stand_ins, asked, _processes) = start(&m).await;
        let op = on_bus(&m, "boot");
        let esp = m.dir.join("root/boot/efi");
        let efivars = || -> Value {
            serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap()
        };

        let plan = op.ok(&["boot", "install", "--dry-run"]);
        for expected in [
            "default legionctl-linux-cachyos-bore-lto.efi",
            "--create-only --disk /dev/nvme1n1 --part 1 --label legionctl",
            "Restart once: the next start uses legionctl's entry one time.",
            // the entry that started this computer, not a boot manager's name (live r340: "rEFInd" for Linpus lite)
            "If it fails, the start after uses Linpus lite as before.",
        ] {
            assert!(plan.contains(expected), "{expected} missing:\n{plan}");
        }
        assert_eq!(
            (efivars()["order"].clone(), asked.lock().unwrap().len()),
            (serde_json::json!(["0003", "0004"]), 0),
            "a dry run changes nothing; it is the daemon's plan, without polkit"
        );

        op.ok(&["boot", "install"]);
        let units = std::fs::read_to_string(m.dir.join("units.log")).unwrap();
        let mut units: Vec<&str> = units.lines().collect();
        units.sort_unstable();
        assert_eq!(
            units,
            ["efibootmgr", "efibootmgr", "sbsign", "ukify", "ukify"],
            "the signers and the efibootmgr changes run as transient units outside the daemon's sandbox"
        );
        let loader = std::fs::read_to_string(esp.join("EFI/legionctl/grubx64.efi")).unwrap();
        assert!(
            loader.contains("sbsign")
                && loader.contains("refind_local.key")
                && loader.contains("systemd-bootx64.efi"),
            "{loader}"
        );
        for kernel in ["linux-cachyos", "linux-cachyos-bore-lto"] {
            let image =
                std::fs::read_to_string(esp.join(format!("EFI/Linux/legionctl-{kernel}.efi")))
                    .unwrap();
            assert!(
                image.contains(&format!("vmlinuz-{kernel}"))
                    && image.contains("intel-ucode.img")
                    && image.contains("--cmdline quiet rw root=UUID=a9e6 --")
                    && image.contains("--secureboot-private-key"),
                "{image}"
            );
        }
        assert!(
            esp.join("EFI/legionctl/shimx64.efi").exists()
                && esp.join("EFI/legionctl/mmx64.efi").exists()
        );
        let vars = efivars();
        assert_eq!(vars["entries"]["0007"], "legionctl");
        assert_eq!(
            vars["order"],
            serde_json::json!(["0003", "0004"]),
            "not first: the old chain still leads"
        );
        assert_eq!(vars["next"], "0007", "the next start tries it once");

        // the default kernel's package goes: loader.conf must not start an image that rebuild removes
        op.ok(&["kernel", "default", "linux-cachyos"]);
        std::fs::remove_file(m.dir.join("root/boot/vmlinuz-linux-cachyos")).unwrap();
        op.ok(&["boot", "rebuild"]);
        assert!(
            !esp.join("EFI/Linux/legionctl-linux-cachyos.efi").exists(),
            "the image of a removed kernel goes"
        );
        let loader = std::fs::read_to_string(esp.join("loader/loader.conf")).unwrap();
        assert!(
            loader.contains("default legionctl-linux-cachyos-bore-lto.efi\n"),
            "the running kernel becomes the default: {loader}"
        );

        op.ok(&["boot", "remove"]);
        let vars = efivars();
        assert_eq!(vars["order"], serde_json::json!(["0003", "0004"]));
        assert!(vars["entries"].get("0007").is_none());
        assert!(
            !esp.join("EFI/legionctl").exists()
                && !esp
                    .join("EFI/Linux/legionctl-linux-cachyos-bore-lto.efi")
                    .exists()
        );
        assert!(!esp.join("loader/loader.conf").exists());
        assert!(
            esp.join("EFI/BOOT/bootx64.efi").exists(),
            "the old chain is as it was"
        );
    }

    /// The package's unit that runs after each start's login screen: its command's arguments.
    fn boot_confirm_unit() -> Vec<String> {
        let unit = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../systemd/legionctl-boot-confirm.service"),
        )
        .unwrap();
        let makefile =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Makefile"))
                .unwrap();
        assert!(
            unit.lines().any(|l| l == "After=graphical.target")
                && makefile.contains("graphical.target.wants/legionctl-boot-confirm.service"),
            "each start runs it after the login screen is up:\n{unit}"
        );
        let exec: Vec<String> = unit
            .lines()
            .find_map(|l| l.strip_prefix("ExecStart="))
            .unwrap()
            .split_whitespace()
            .map(String::from)
            .collect();
        assert_eq!(exec[0], "/usr/lib/legionctl/legionctl-system");
        exec[1..].to_vec()
    }

    /// todo-180 G (the user, r324: "wtf is boot confirm"): the start through legionctl's entry makes it the default by
    /// itself once the login screen is up (the package's unit after graphical.target), and the desktop says so once.
    /// A start through another entry (the firmware went back to rEFInd) changes nothing and doctor says what to do; a
    /// start through the entry when it is the default already changes nothing. A restart into Windows says that it
    /// replaces the waiting start (U-11: one owner of `BootNext`).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_good_start_through_legionctls_entry_makes_it_the_default_and_tells_the_desktop() {
        let m = Machine::new("bootgood");
        let state = boot_machine(&m);
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let address = format!("unix:path={}", m.dir.join("bus").display());
        let (_desktop, told) = crate::desktop::answering(&address, &[]).await;
        let op = Operator::start(
            "bootgood",
            &[
                ("PATH", m.dir.join("bin").into()),
                ("HOME", m.dir.join("home").into()),
                ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
                ("DBUS_SYSTEM_BUS_ADDRESS", address.clone().into()),
                ("DBUS_SESSION_BUS_ADDRESS", address.into()),
            ],
        );
        let exec = boot_confirm_unit();
        let started = |current: &str| {
            let mut vars: Value =
                serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
            vars["current"] = current.into();
            vars["next"] = Value::Null;
            std::fs::write(&state, vars.to_string()).unwrap();
            let args: Vec<&str> = exec.iter().map(String::as_str).collect();
            let out = one_shot(&m, &args);
            assert!(out.status.success(), "{out:?}");
        };
        let order = || -> Value {
            serde_json::from_str::<Value>(&std::fs::read_to_string(&state).unwrap()).unwrap()["order"].clone()
        };
        let marker = m.dir.join("root/run/legionctl/boot-confirmed");

        started("0003");
        assert!(!marker.exists(), "no takeover: nothing");
        let install = op.ok(&["boot", "install"]);
        assert!(
            install.contains("If the desktop comes up, it becomes the default by itself")
                && !install.contains("confirm"),
            "{install}"
        );
        let windows = op.ok(&["windows", "reboot", "--dry-run"]);
        assert!(
            windows
                .contains("this restart replaces the waiting start through legionctl's boot entry"),
            "BootNext has one owner: {windows}"
        );

        started("0003");
        assert_eq!(
            order(),
            serde_json::json!(["0003", "0004"]),
            "the firmware went back to rEFInd"
        );
        assert!(!marker.exists());
        let doctor = String::from_utf8(op.cli(&["doctor"]).stdout).unwrap();
        assert!(
            doctor.contains(
                "the last start did not use legionctl's boot entry (it used Linpus lite), so Linpus lite stays \
                 the default"
            ) && doctor
                .contains("To try legionctl's entry again: legionctl boot install, then restart"),
            "{doctor}"
        );

        started("0007");
        assert_eq!(order(), serde_json::json!(["0007", "0003", "0004"]));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(40);
        while told.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        let doctor = String::from_utf8(op.cli(&["doctor"]).stdout).unwrap();
        assert!(!doctor.contains("boot entry"), "{doctor}");
        assert!(
            op.ok(&["boot", "status"])
                .contains("legionctl's entry: 0007 legionctl, the default")
        );

        started("0007");
        let writes = std::fs::read_to_string(format!("{}.log", state.display())).unwrap();
        assert_eq!(
            writes.matches("--bootorder").count(),
            1,
            "the default is left alone:\n{writes}"
        );
        // two more checks of the session module (30 s each): one notice for this start
        tokio::time::sleep(std::time::Duration::from_secs(31)).await;
        assert_eq!(
            *told.lock().unwrap(),
            ["legionctl's boot entry worked and is now the default."]
        );
    }

    /// todo-149 (the user's run 2026-10-16: boot install stopped at a missing ukify after it signed systemd-boot): the
    /// plan checks each tool that it runs, in the dry run too, and refuses with the package before any step; an install
    /// that fails on the way (ukify fails) leaves no firmware entry, and the next install completes it; one more install
    /// makes no second firmware entry.
    #[tokio::test(flavor = "multi_thread")]
    async fn boot_install_needs_its_tools_first_and_the_next_install_completes_a_failed_one() {
        let m = Machine::new("boot-tools");
        let state = boot_machine(&m);
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "boot-tools");
        let (bin, esp) = (m.dir.join("bin"), m.dir.join("root/boot/efi"));
        let efivars = || -> Value {
            serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap()
        };
        let creates = || {
            std::fs::read_to_string(format!("{}.log", state.display()))
                .unwrap_or_default()
                .matches("--create-only")
                .count()
        };
        let ukify = std::fs::read_to_string(bin.join("ukify")).unwrap();
        let sbverify = std::fs::read_to_string(bin.join("sbverify")).unwrap();
        std::fs::remove_file(bin.join("ukify")).unwrap();
        std::fs::remove_file(bin.join("sbverify")).unwrap();
        for args in [&["boot", "install", "--dry-run"][..], &["boot", "install"]] {
            let out = op.cli(args);
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(
                !out.status.success()
                    && err.contains("ukify (systemd-ukify), sbverify (sbsigntools)")
                    && err.contains("pacman -S systemd-ukify sbsigntools"),
                "{args:?}: {err}"
            );
        }
        assert!(
            !esp.join("EFI/legionctl").exists() && !m.dir.join("units.log").exists(),
            "no step runs before the refusal"
        );

        Machine::script(&bin, "sbverify", sbverify.trim_start_matches("#!/bin/sh\n"));
        Machine::script(&bin, "ukify", "echo 'ukify: build failed' >&2; exit 1");
        let out = op.cli(&["boot", "install"]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success() && err.contains("ukify: build failed"),
            "{err}"
        );
        assert!(
            esp.join("EFI/legionctl/grubx64.efi").exists(),
            "the steps before ukify ran"
        );
        assert_eq!(creates(), 0, "no firmware entry after a failed install");

        Machine::script(&bin, "ukify", ukify.trim_start_matches("#!/bin/sh\n"));
        op.ok(&["boot", "install"]);
        op.ok(&["boot", "install"]);
        let vars = efivars();
        assert_eq!(
            (
                creates(),
                vars["entries"]["0007"].clone(),
                vars["next"].clone()
            ),
            (1, Value::from("legionctl"), Value::from("0007")),
            "one firmware entry, tried once: {vars}"
        );
        for kernel in ["linux-cachyos", "linux-cachyos-bore-lto"] {
            let image =
                std::fs::read_to_string(esp.join(format!("EFI/Linux/legionctl-{kernel}.efi")))
                    .unwrap();
            assert!(image.contains("--secureboot-private-key"), "{image}");
        }
    }

    /// The recorded kernels' module lists, cut to what the kernel lines name: `zswap` is built in (its `enabled`
    /// parameter is not in modinfo, as on the machine) and `nvidia-drm` is a DKMS module.
    fn kernel_modules(m: &Machine) {
        for release in ["7.2.9-2-cachyos", "7.2.9-2-cachyos-bore-lto"] {
            let dir = m.dir.join("root/usr/lib/modules").join(release);
            std::fs::write(
                dir.join("modules.builtin.modinfo"),
                "zswap.parmtype=max_pool_percent:uint\0zswap.description=Compressed cache for swap pages\0",
            )
            .unwrap();
            std::fs::write(
                dir.join("modules.dep"),
                "updates/dkms/nvidia-drm.ko.zst: updates/dkms/nvidia-modeset.ko.zst updates/dkms/nvidia.ko.zst\n",
            )
            .unwrap();
        }
    }

    /// The EFI variable `LoaderEntryOneShot` as systemd-boot reads it, or `None`.
    fn oneshot(m: &Machine) -> Option<String> {
        let bytes = std::fs::read(m.dir.join(
            "root/sys/firmware/efi/efivars/LoaderEntryOneShot-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f",
        ))
        .ok()?;
        assert_eq!(bytes[..4], [7, 0, 0, 0], "non-volatile, boot and run time");
        let text: Vec<u16> = bytes[4..]
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(text.last(), Some(&0), "UTF-16 with its end mark");
        Some(String::from_utf16(&text[..text.len() - 1]).unwrap())
    }

    /// `kernel list`: whether the boot takeover is installed, and package -> release, flavour, running, default,
    /// DKMS, image, command line.
    fn kernel_list(op: &Operator) -> (bool, Value) {
        let all: Value = serde_json::from_str(&op.ok(&["kernel", "list", "--json"])).unwrap();
        let fields = [
            "release", "flavour", "running", "default", "dkms", "image", "cmdline",
        ];
        let kernels = all["kernels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| {
                let key = k["package"].as_str().unwrap().to_string();
                (key, fields.iter().map(|f| k[*f].clone()).collect::<Value>())
            })
            .collect::<serde_json::Map<_, _>>();
        (all["takeover"].as_bool().unwrap(), kernels.into())
    }

    /// Kernels (todo-84 part 1) on the recorded machine: `kernel list` shows both installed kernels with their
    /// scheduler, the DKMS state of nvidia and legionctl-wmi, which runs and which is the default; `default` and `try`
    /// are refused, with nothing changed, until the boot takeover is installed; then `default` changes legionctl's
    /// `loader.conf` only, and `try` sets the one-time entry and `BootNext` and restarts, while the default stays.
    #[tokio::test(flavor = "multi_thread")]
    async fn kernels_are_listed_and_start_through_the_boot_takeover_once_or_by_default() {
        let m = Machine::new("kernels");
        let state = boot_machine(&m);
        let bin = m.dir.join("bin");
        Machine::script(
            &bin,
            "dkms",
            "printf 'nvidia/580.178.04, 7.2.9-2-cachyos-bore-lto, x86_64: installed\\n\
             nvidia/580.178.04, 7.2.9-2-cachyos, x86_64: installed\\n\
             legionctl-wmi/0.1.0, 7.2.9-2-cachyos-bore-lto, x86_64: installed\\n'",
        );
        let restarts = m.dir.join("systemctl.log");
        Machine::script(
            &bin,
            "systemctl",
            &format!(
                "[ \"$1\" = reboot ] && echo reboot >> {}; exit 0",
                restarts.display()
            ),
        );
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "kernels");
        let esp = m.dir.join("root/boot/efi");
        let efivars = || -> Value {
            serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap()
        };
        assert!(!kernel_list(&op).0, "no boot takeover yet");
        for args in [
            ["kernel", "default", "linux-cachyos"],
            ["kernel", "try", "linux-cachyos"],
        ] {
            let refused = op.cli(&args);
            assert!(
                String::from_utf8_lossy(&refused.stderr)
                    .contains("install the boot takeover first (legionctl boot install)"),
                "{args:?}: {refused:?}"
            );
        }
        assert!(!esp.join("loader/loader.conf").exists());
        assert_eq!(efivars()["next"], Value::Null);
        assert_eq!(oneshot(&m), None);
        assert!(!restarts.exists(), "no restart without the takeover");

        op.ok(&["boot", "install"]);
        let shown = || {
            let (takeover, kernels) = kernel_list(&op);
            assert!(takeover);
            kernels
        };
        // the flavour is the configuration's, not the name's: linux-cachyos is LTO too
        let expected = |plain_default: bool| {
            serde_json::json!({
                "linux-cachyos": ["7.2.9-2-cachyos", ["eevdf", "lto"], false, plain_default,
                    ["nvidia/580.178.04: installed", "legionctl-wmi: not built"],
                    "legionctl-linux-cachyos.efi", "quiet rw root=UUID=a9e6"],
                "linux-cachyos-bore-lto": ["7.2.9-2-cachyos-bore-lto", ["bore", "lto"], true, !plain_default,
                    ["nvidia/580.178.04: installed", "legionctl-wmi/0.1.0: installed"],
                    "legionctl-linux-cachyos-bore-lto.efi", "quiet rw root=UUID=a9e6"],
            })
        };
        assert_eq!(shown(), expected(false));

        // forget install's own try, to see which change sets BootNext
        let mut vars = efivars();
        vars["next"] = Value::Null;
        std::fs::write(&state, vars.to_string()).unwrap();
        let loader = || std::fs::read_to_string(esp.join("loader/loader.conf")).unwrap();
        let dry = op.ok(&["kernel", "default", "linux-cachyos", "--dry-run"]);
        assert!(dry.contains("default legionctl-linux-cachyos.efi"), "{dry}");
        assert!(loader().contains("default legionctl-linux-cachyos-bore-lto.efi"));
        op.ok(&["kernel", "default", "linux-cachyos"]);
        assert!(loader().contains("default legionctl-linux-cachyos.efi\n"));
        assert_eq!(
            efivars()["next"],
            Value::Null,
            "default is not a one-time start"
        );
        assert_eq!(oneshot(&m), None);
        assert!(!restarts.exists());
        assert_eq!(shown(), expected(true));

        let refused = op.cli(&["kernel", "try", "linux-zen"]);
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("no kernel linux-zen in /boot"),
            "{refused:?}"
        );
        op.ok(&["kernel", "try", "linux-cachyos-bore-lto"]);
        assert_eq!(
            oneshot(&m).as_deref(),
            Some("legionctl-linux-cachyos-bore-lto.efi")
        );
        assert_eq!(
            efivars()["next"],
            "0007",
            "the next start goes through legionctl's entry, which rEFInd may still lead"
        );
        assert_eq!(std::fs::read_to_string(&restarts).unwrap(), "reboot\n");
        assert!(
            loader().contains("default legionctl-linux-cachyos.efi\n"),
            "the default stays"
        );
    }

    /// The start of this boot through legionctl's image `image`, as systemd-boot leaves it: `LoaderEntrySelected` is
    /// the image, and the one-time entry is used up.
    fn start_through(m: &Machine, image: &str) {
        let vars = m.dir.join("root/sys/firmware/efi/efivars");
        let mut bytes = vec![6, 0, 0, 0];
        bytes.extend(image.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
        std::fs::write(
            vars.join("LoaderEntrySelected-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f"),
            bytes,
        )
        .unwrap();
        let _ = std::fs::remove_file(
            vars.join("LoaderEntryOneShot-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f"),
        );
    }

    /// `command` (`kernel cmdline set` or `undo` of linux-cachyos, to `line`) two times before a start through the
    /// trial image: the trial image has `line` and is the next start's only; linux-cachyos's image and line stay.
    fn trial_only(m: &Machine, op: &Operator, command: &[&str], line: &str) {
        let image = |name: &str| {
            std::fs::read_to_string(m.dir.join(format!("root/boot/efi/EFI/Linux/{name}"))).unwrap()
        };
        let kernel = || {
            (
                image("legionctl-linux-cachyos.efi"),
                m.file("/etc/legionctl/kernel/linux-cachyos.cmdline"),
            )
        };
        let before = kernel();
        let first = op.ok(command);
        assert!(
            first.contains("the next start only uses the new line"),
            "{first}"
        );
        let trial = image("legionctl-linux-cachyos.trial.efi");
        assert!(trial.contains(&format!("--cmdline {line} --")), "{trial}");
        assert_eq!(
            oneshot(m).as_deref(),
            Some("legionctl-linux-cachyos.trial.efi")
        );
        assert_eq!(kernel(), before, "the kernel's image and line stay");
        op.ok(command);
        assert_eq!(
            kernel(),
            before,
            "the same command again without a start through the trial image changes nothing of the kernel"
        );
    }

    /// A kernel's own command line (todo-84 part 2): it goes into that kernel's image only, signed and checked, and
    /// only after one start through a trial image (the next start only, as `kernel try`; the same command after that
    /// start makes it the kernel's), so a bad line never replaces the default image; a line without root=, with a
    /// parameter of a module the kernel does not have, or an image that is not signed is refused with the old image
    /// and line left; `boot rebuild` keeps each kernel's line (it is the one source), and undo puts the line before
    /// back the same way.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_kernels_command_line_goes_into_its_image_only_and_undo_puts_the_old_one_back() {
        const LINE: &str = "quiet mitigations=off nvidia-drm.modeset=1 clocksource=tsc tsc=invariant \
                            zswap.enabled=0 nowatchdog splash rw root=UUID=a9e6 resume=UUID=c909";
        let m = Machine::new("cmdline");
        boot_machine(&m);
        kernel_modules(&m);
        m.set(
            "/proc/cmdline",
            &format!("{LINE} initrd=\\initramfs-linux-cachyos-bore-lto.img"),
        );
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "cmdline");
        let linux = m.dir.join("root/boot/efi/EFI/Linux");
        let image = |kernel: &str| {
            std::fs::read_to_string(linux.join(format!("legionctl-{kernel}.efi"))).unwrap()
        };
        let saved = || m.file("/etc/legionctl/kernel/linux-cachyos.cmdline");

        op.ok(&["boot", "install"]);
        assert!(
            image("linux-cachyos").contains(&format!("--cmdline {LINE} --")),
            "the running line, with a built-in and a DKMS module's parameter: {}",
            image("linux-cachyos")
        );
        assert_eq!(saved(), LINE);
        let bore = image("linux-cachyos-bore-lto");

        let mine = LINE.replace("mitigations=off ", "");
        let set = ["kernel", "cmdline", "set", "linux-cachyos", &mine];
        trial_only(&m, &op, &set, &mine);
        start_through(&m, "legionctl-linux-cachyos.trial.efi");
        op.ok(&set);
        assert!(
            image("linux-cachyos").contains(&format!("--cmdline {mine} --")),
            "{}",
            image("linux-cachyos")
        );
        assert_eq!(saved(), mine);
        assert!(!linux.join("legionctl-linux-cachyos.trial.efi").exists());
        assert_eq!(
            image("linux-cachyos-bore-lto"),
            bore,
            "the other kernel's image is as it was"
        );

        let ours = image("linux-cachyos");
        for (line, why) in [
            ("quiet rw resume=UUID=c909", "has no root="),
            (
                "quiet rw zwsap.enabled=0 root=UUID=a9e6",
                "zwsap.enabled=0: linux-cachyos (7.2.9-2-cachyos) has no module zwsap",
            ),
        ] {
            let refused = op.cli(&["kernel", "cmdline", "set", "linux-cachyos", line]);
            assert!(
                String::from_utf8_lossy(&refused.stderr).contains(why),
                "{why}: {refused:?}"
            );
            assert_eq!(image("linux-cachyos"), ours, "nothing written: {line}");
            assert_eq!(saved(), mine);
        }

        // an image that ukify did not sign fails its check, and the old one stays
        let bin = m.dir.join("bin");
        let signer = std::fs::read_to_string(bin.join("ukify")).unwrap();
        Machine::script(
            &bin,
            "ukify",
            "out=; prev=; for a in \"$@\"; do [ \"$prev\" = --output ] && out=$a; prev=$a; done; echo unsigned > \"$out\"",
        );
        let refused = op.cli(&["kernel", "cmdline", "set", "linux-cachyos", LINE]);
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("sbverify"),
            "{refused:?}"
        );
        assert_eq!(image("linux-cachyos"), ours);
        assert_eq!(saved(), mine);
        std::fs::write(bin.join("ukify"), signer).unwrap();

        op.ok(&["boot", "rebuild"]);
        assert!(
            image("linux-cachyos").contains(&format!("--cmdline {mine} --")),
            "rebuild keeps the kernel's own line: {}",
            image("linux-cachyos")
        );

        // undo also goes through one start: this start came through the earlier trial image, but the new one-time
        // entry is not used yet, so the second undo makes the trial image again and changes nothing of the kernel
        let undo = ["kernel", "cmdline", "undo", "linux-cachyos"];
        trial_only(&m, &op, &undo, LINE);
        start_through(&m, "legionctl-linux-cachyos.trial.efi");
        op.ok(&undo);
        assert!(
            image("linux-cachyos").contains(&format!("--cmdline {LINE} --")),
            "{}",
            image("linux-cachyos")
        );
        assert_eq!(saved(), LINE);
        assert_eq!(image("linux-cachyos-bore-lto"), bore);
    }

    /// The machine's restarts so far (the stand-in systemctl logs each `reboot`).
    fn restarts(m: &Machine) -> usize {
        std::fs::read_to_string(m.dir.join("systemctl.log"))
            .unwrap_or_default()
            .lines()
            .count()
    }

    /// The newest job of `kind` (`jobs list`).
    fn last_job(op: &Operator, kind: &str) -> Value {
        let jobs: Value = serde_json::from_str(&op.ok(&["jobs", "list", "--json"])).unwrap();
        jobs["items"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|j| j["kind"] == kind)
            .cloned()
            .unwrap_or(Value::Null)
    }

    const OSRELEASE: &str = "/proc/sys/kernel/osrelease";
    const SERIES: [&str; 6] = ["kernel", "bench", "cpu", "--all", "--seconds", "1"];

    /// The boot machine with `boot install` done, a systemctl that logs each restart, and a stand-in stress-ng that
    /// scores by the running release (40000 on bore-lto, 36000 else), so each run shows the kernel it really ran on.
    async fn series_machine(
        tag: &'static str,
    ) -> (Machine, zbus::Connection, (Kill, Kill), Operator) {
        let m = Machine::new(tag);
        boot_machine(&m);
        let bin = m.dir.join("bin");
        Machine::script(
            &bin,
            "systemctl",
            &format!(
                "[ \"$1\" = reboot ] && echo reboot >> {}; exit 0",
                m.dir.join("systemctl.log").display()
            ),
        );
        Machine::script(
            &bin,
            "stress-ng",
            &format!(
                "/usr/bin/sleep 1.2; read -r r < {}; case $r in *bore-lto) s=40000.00;; *) s=36000.00;; esac; \
                 echo \"stress-ng: metrc: [1] cpu              114043      3.00     91.33      0.08     $s        1247.63\" >&2",
                m.dir.join("root").join(&OSRELEASE[1..]).display()
            ),
        );
        let (stand_ins, _asked, processes) = start(&m).await;
        let op = on_bus(&m, tag);
        op.ok(&["boot", "install"]);
        (m, stand_ins, processes, op)
    }

    /// A start into `release`: the operator starts again at login (`graphical-session.target`; here its socket's
    /// first call).
    fn login(m: &Machine, tag: &'static str, op: Operator, release: &str) -> Operator {
        drop(op);
        m.set(OSRELEASE, release);
        let op = on_bus(m, tag);
        op.ok(&["jobs", "list"]);
        op
    }

    fn bench_runs(op: &Operator) -> Vec<Value> {
        let page: Value = serde_json::from_str(&op.ok(&["bench", "list", "--json"])).unwrap();
        page["items"].as_array().unwrap().clone()
    }

    /// A bench series across the kernels (todo-84 part 3), with the real daemon and operator on the recorded machine
    /// after `boot install`: each kernel starts once through `kernel try`; the operator (it starts at login) checks
    /// that the kernel it asked for runs, benches it, records the kernel with the run, and starts the next one, and at
    /// the end the default kernel once more. The view and `bench compare` show the change between the kernels.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_bench_series_starts_each_kernel_once_and_goes_on_after_each_start() {
        let (m, _stand_ins, _processes, op) = series_machine("series").await;
        let dry = op.ok(&[&SERIES[..], &["--dry-run"]].concat());
        assert_eq!(
            dry.lines().collect::<Vec<_>>(),
            [
                "start linux-cachyos once (kernel try: a restart), then bench cpu 1 s after login",
                "start linux-cachyos-bore-lto once (kernel try: a restart), then bench cpu 1 s after login",
                "start linux-cachyos-bore-lto once more (the default kernel); the series ends",
            ],
            "{dry}"
        );
        assert_eq!(
            (oneshot(&m), restarts(&m)),
            (None, 0),
            "a dry run changes nothing"
        );

        op.ok(&SERIES);
        assert_eq!(oneshot(&m).as_deref(), Some("legionctl-linux-cachyos.efi"));
        assert_eq!(restarts(&m), 1);
        let again = op.cli(&SERIES);
        assert!(
            String::from_utf8_lossy(&again.stderr).contains("runs already"),
            "{again:?}"
        );

        let op = login(&m, "series", op, "7.2.9-2-cachyos");
        wait_for("the start of the next kernel", || restarts(&m) == 2);
        assert_eq!(
            oneshot(&m).as_deref(),
            Some("legionctl-linux-cachyos-bore-lto.efi")
        );
        let first = bench_runs(&op);
        assert_eq!(
            (
                first.len(),
                &first[0]["kernel"],
                &first[0]["release"],
                &first[0]["scores"]["cpu bogo ops/s"]
            ),
            (
                1,
                &Value::from("linux-cachyos"),
                &Value::from("7.2.9-2-cachyos"),
                &Value::from(36000.0)
            ),
            "{first:?}"
        );

        let op = login(&m, "series", op, "7.2.9-2-cachyos-bore-lto");
        wait_for("the start of the default kernel", || restarts(&m) == 3);
        assert_eq!(
            oneshot(&m).as_deref(),
            Some("legionctl-linux-cachyos-bore-lto.efi")
        );
        assert_eq!(last_job(&op, "kernel.bench")["state"], "running");

        let op = login(&m, "series", op, "7.2.9-2-cachyos-bore-lto");
        wait_for("the end of the series", || {
            last_job(&op, "kernel.bench")["state"] == "done"
        });
        assert_eq!(restarts(&m), 3, "no restart after the last start");
        let all = bench_runs(&op);
        assert_eq!(all.len(), 2, "the last start has no bench: {all:?}");
        assert_eq!(all[1]["kernel"], "linux-cachyos-bore-lto");

        let view = op.ok(&["kernel", "bench", "cpu"]);
        assert!(
            view.lines().any(|l| l.starts_with("cpu bogo ops/s")
                && l.contains("36000")
                && l.ends_with("40000 +11.1%")),
            "{view}"
        );
        let (a, b) = (all[0]["id"].to_string(), all[1]["id"].to_string());
        let compare = op.ok(&["bench", "compare", &a, &b]);
        assert!(
            compare.contains("on linux-cachyos (7.2.9-2-cachyos)")
                && compare.contains("on linux-cachyos-bore-lto (7.2.9-2-cachyos-bore-lto)")
                && compare
                    .lines()
                    .any(|l| l.starts_with("cpu bogo ops/s") && l.ends_with("+11.1%")),
            "{compare}"
        );
    }

    /// A series stops when the kernel that started is not the one it asked for (no bench, no other start), and a
    /// cancelled series does nothing at the next start; the view shows the newest run of each kernel.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_bench_series_stops_at_a_wrong_start_and_when_cancelled() {
        let (m, _stand_ins, _processes, op) = series_machine("stops").await;
        let older: Value =
            serde_json::from_str(&op.ok(&["bench", "run", "cpu", "--seconds", "1", "--json"]))
                .unwrap();

        // the start of linux-cachyos fails: the default kernel starts, and the series stops there
        op.ok(&SERIES);
        assert_eq!(restarts(&m), 1);
        let op = login(&m, "stops", op, "7.2.9-2-cachyos-bore-lto");
        wait_for("the series to stop", || {
            last_job(&op, "kernel.bench")["state"] == "failed"
        });
        let job = last_job(&op, "kernel.bench");
        assert!(
            job["error"]
                .as_str()
                .unwrap()
                .starts_with("linux-cachyos did not start (linux-cachyos-bore-lto runs)"),
            "{job}"
        );
        assert_eq!(
            (restarts(&m), bench_runs(&op).len()),
            (1, 1),
            "no bench, no start"
        );

        op.ok(&SERIES);
        assert_eq!(restarts(&m), 2);
        let cancel = op.ok(&["kernel", "bench", "cpu", "--cancel"]);
        assert!(
            cancel.contains("stopped before its bench on linux-cachyos"),
            "{cancel}"
        );
        let op = login(&m, "stops", op, "7.2.9-2-cachyos");
        std::thread::sleep(std::time::Duration::from_secs(3));
        assert_eq!((restarts(&m), bench_runs(&op).len()), (2, 1));
        assert_eq!(last_job(&op, "kernel.bench")["error"], "you cancelled it");

        m.set(OSRELEASE, "7.2.9-2-cachyos-bore-lto");
        let newer: Value =
            serde_json::from_str(&op.ok(&["bench", "run", "cpu", "--seconds", "1", "--json"]))
                .unwrap();
        let view = op.ok(&["kernel", "bench", "cpu"]);
        assert!(
            view.contains(&format!(
                "run {} cpu 1 s on linux-cachyos-bore-lto (",
                newer["id"]
            )) && !view.contains(&format!("run {} ", older["id"])),
            "{view}"
        );
    }

    /// A tune result for one kernel (todo-84 part 3): `tune --per-kernel` writes the generated layer
    /// `tuned-<package>` with the `kernel` condition of the running kernel, not the plain `tuned` layer; it is off
    /// until `rules enable` (and, as each power layer, until a layer gives the battery value), then it sets the limit
    /// on its kernel only.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_tuned_layer_for_one_kernel_applies_only_on_that_kernel() {
        let m = Machine::new("tunekernel");
        // the sweep's thermal guard reads the package temperature
        m.set("/sys/class/hwmon/hwmon8/name", "coretemp");
        m.set("/sys/class/hwmon/hwmon8/temp1_input", "55000");
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
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "tunekernel");
        let out = op.cli(&[
            "tune",
            "--per-kernel",
            "--seconds",
            "1",
            "--cooldown-s",
            "0",
            "ppt_pl1_spl",
            "cpu",
            "90",
            "60",
            "80",
        ]);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("recommended ppt_pl1_spl = 80"),
            "{text}{out:?}"
        );
        assert_eq!(
            m.file("/var/lib/legionctl/configsets/50-tuned.jsonc"),
            "<missing>",
            "not the layer for every kernel"
        );
        let layer = m.file("/var/lib/legionctl/configsets/50-tuned-linux-cachyos-bore-lto.jsonc");
        let json: Value = serde_json::from_str(layer.split_once('\n').unwrap().1).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"when": {"power": "ac", "kernel": "linux-cachyos-bore-lto"},
                               "set": {"firmware.ppt_pl1_spl": 80}}),
            "{layer}"
        );
        assert_eq!(m.file(PL1), "90", "off until enabled");

        // a power layer is used only with a value for the other power source too
        op.ok(&[
            "rules",
            "edit",
            "40-battery-pl1",
            "firmware.ppt_pl1_spl=45",
            "--when",
            "power=battery",
            "--system",
        ]);
        op.ok(&["rules", "enable", "tuned-linux-cachyos-bore-lto"]);
        wait_for("the layer on its kernel", || m.file(PL1) == "80");
        // a restart into the other kernel: the firmware has its own value again
        let restart = |release: &str| {
            m.set("/proc/sys/kernel/osrelease", release);
            m.set(PL1, "90");
            op.ok(&["power", "apply"]);
            m.file(PL1)
        };
        assert_eq!(restart("7.2.9-2-cachyos"), "90", "not on linux-cachyos");
        let rules = op.ok(&["rules", "list"]);
        assert!(
            rules.lines().any(
                |l| l.starts_with("generated/50-tuned-linux-cachyos-bore-lto")
                    && l.ends_with("on, waits for its conditions")
            ),
            "{rules}"
        );
        assert_eq!(restart("7.2.9-2-cachyos-bore-lto"), "80");
    }

    /// A stand-in of the `linux-cachyos` PKGBUILD repository at `dir`: the folder `linux-cachyos-bore` in three
    /// versions, 7.2.9-1, 7.2.9-2 (the installed one) and 7.2.10-1. Each PKGBUILD names its package as the real one
    /// does and writes its version and name into the source tree in `prepare()`.
    fn cachyos_repo(dir: &Path) {
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-C"])
                .arg(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
        };
        let folder = dir.join("linux-cachyos-bore");
        std::fs::create_dir_all(&folder).unwrap();
        git(&["init", "--quiet"]);
        for (ver, rel) in [("7.2.9", "1"), ("7.2.9", "2"), ("7.2.10", "1")] {
            std::fs::write(
                folder.join(".SRCINFO"),
                format!("pkgbase = linux-cachyos-bore\n\tpkgver = {ver}\n\tpkgrel = {rel}\n"),
            )
            .unwrap();
            std::fs::write(
                folder.join("PKGBUILD"),
                format!(
                    ": \"${{_use_llvm_lto:=none}}\"\n_cpusched=bore\n\
                     if [ \"$_use_llvm_lto\" = thin ]; then _pkgsuffix=cachyos-${{_cpusched}}-lto; else _pkgsuffix=cachyos-${{_cpusched}}; fi\n\
                     pkgbase=\"linux-$_pkgsuffix\"\npkgver={ver}\npkgrel={rel}\n_srcname=linux-{ver}\nBUILD_FLAGS=(LLVM=1)\n\
                     prepare() {{\n    cd \"$_srcname\"\n    echo \"-$pkgrel\" > localversion.10-pkgrel\n    \
                     echo \"${{pkgbase#linux}}\" > localversion.20-pkgname\n    echo CONFIG_SCHED_BORE=y > .config\n}}\n"
                ),
            )
            .unwrap();
            git(&["add", "."]);
            git(&["commit", "--quiet", "-m", &format!("{ver}-{rel}")]);
        }
    }

    /// The boot machine with `boot install` done and the stand-ins of a kernel build: git clones a local copy of the
    /// repository (`cachyos_repo`), docker runs the PKGBUILD's `prepare()` (with stand-in `merge_config.sh` and make)
    /// and writes the packages (the first run hangs while the file `hang` exists), pacman logs. The logs are in the
    /// machine's folder.
    async fn build_machine(
        tag: &'static str,
    ) -> (
        Machine,
        zbus::Connection,
        daemon_process::Asked,
        (Kill, Kill),
        Operator,
    ) {
        let m = Machine::new(tag);
        boot_machine(&m);
        let bin = m.dir.join("bin");
        let repo = m.dir.join("cachyos");
        cachyos_repo(&repo);
        Machine::script(
            &bin,
            "git",
            &format!(
                "if [ \"$3\" = clone ]; then echo \"$5\" >> {d}/git.log; exec /usr/bin/git clone --quiet {r} \"$6\"; fi; \
                 exec /usr/bin/git \"$@\"",
                d = m.dir.display(),
                r = repo.display()
            ),
        );
        let tools = m.dir.join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        Machine::script(
            &tools,
            "make",
            &format!("echo \"make $*\" >> {}/kconfig.log", m.dir.display()),
        );
        Machine::script(
            &tools,
            "merge_config.sh",
            &format!(
                "echo \"merge $*\" >> {d}/kconfig.log; cat \"$3\" >> {d}/kconfig.log",
                d = m.dir.display()
            ),
        );
        Machine::script(
            &bin,
            "docker",
            &format!(
                "echo \"$*\" >> {d}/docker.log\n\
                 [ \"$1\" = run ] || exit 0\n\
                 if [ -e {d}/hang ]; then /usr/bin/rm {d}/hang; : > {d}/started; exec /usr/bin/sleep 30; fi\n\
                 src=; env=; prev=; for a; do [ \"$prev\" = -v ] && src=${{a%:/build}}; [ \"$prev\" = -e ] && env=$a; prev=$a; done\n\
                 cd \"$src\" && exec /usr/bin/env PATH={t}:/usr/bin \"$env\" /usr/bin/bash -c 'set -e; source ./PKGBUILD; \
                 startdir=$PWD; srcdir=$PWD/src; mkdir -p \"$srcdir/$_srcname/scripts/kconfig\"; \
                 cp {t}/merge_config.sh \"$srcdir/$_srcname/scripts/kconfig/\"; cd \"$srcdir\"; prepare; cd \"$startdir\"; \
                 for p in $pkgbase $pkgbase-headers; do \
                 printf \"pkgname = %s\\npkgbase = %s\\npkgver = %s\\npkgdesc = built\\n\" $p $pkgbase $pkgver-$pkgrel > .PKGINFO; \
                 /usr/bin/bsdtar --zstd -cf $p-$pkgver-$pkgrel-x86_64.pkg.tar.zst .PKGINFO; done; rm .PKGINFO'",
                d = m.dir.display(),
                t = tools.display()
            ),
        );
        pacman_stand_in(&m);
        std::fs::write(
            m.dir.join("rt.config"),
            "CONFIG_PREEMPT_RT=y\n# CONFIG_DEBUG_INFO is not set\n",
        )
        .unwrap();
        let (stand_ins, asked, processes) = start(&m).await;
        let op = on_bus(&m, tag);
        op.ok(&["boot", "install"]);
        (m, stand_ins, asked, processes, op)
    }

    /// A stand-in pacman: one line in `pacman.log` for each package it gets: the file, and `pkgname`, `pkgver` and
    /// `pkgdesc` of its `.PKGINFO`.
    fn pacman_stand_in(m: &Machine) {
        Machine::script(
            &m.dir.join("bin"),
            "pacman",
            &format!(
                "for a; do case $a in *.pkg.tar.zst) /usr/bin/bsdtar -xOf \"$a\" .PKGINFO | /usr/bin/awk -v f=\"$a\" \
                 '{{v[$1]=$3}} END {{print f, v[\"pkgname\"], v[\"pkgver\"], v[\"pkgdesc\"]}}' >> {}/pacman.log;; esac; done",
                m.dir.display()
            ),
        );
    }

    /// A package `file` in `dir` whose `.PKGINFO` has `pkgname`, `pkgbase`, `pkgver` and `pkgdesc`.
    fn package(dir: &Path, file: &str, [name, base, version, desc]: [&str; 4]) {
        let work = dir.join(format!(".{file}"));
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(
            work.join(".PKGINFO"),
            format!("pkgname = {name}\npkgbase = {base}\npkgver = {version}\npkgdesc = {desc}\n"),
        )
        .unwrap();
        let out = std::process::Command::new("/usr/bin/bsdtar")
            .args(["--zstd", "-cf"])
            .arg(dir.join(file))
            .arg("-C")
            .arg(&work)
            .arg(".PKGINFO")
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        std::fs::remove_dir_all(work).unwrap();
    }

    /// Packages of earlier builds in the PKGBUILD's folder `dir`: an older version, and this version with another
    /// fragment (its record).
    fn stale_packages(dir: &Path) {
        let rt = "linux-cachyos-rt";
        for version in ["7.2.9-1", "7.2.9-2"] {
            package(
                dir,
                &format!("{rt}-{version}-x86_64.pkg.tar.zst"),
                [rt, rt, version, "stale"],
            );
        }
        std::fs::write(dir.join("legionctl.built"), "7.2.9-2\nCONFIG_HZ_1000=y\n").unwrap();
    }

    /// pacman got root's copies of this build's packages (7.2.9-2, `built`) only, and the stale ones are gone.
    fn only_this_build_installed(m: &Machine, dir: &Path) {
        assert_eq!(
            log(m, "pacman.log"),
            format!(
                "{d}/linux-cachyos-rt-7.2.9-2-x86_64.pkg.tar.zst linux-cachyos-rt 7.2.9-2 built\n\
             {d}/linux-cachyos-rt-headers-7.2.9-2-x86_64.pkg.tar.zst linux-cachyos-rt-headers 7.2.9-2 built\n",
                d = m
                    .dir
                    .join("root/var/lib/legionctl/kernel-install")
                    .display()
            ),
            "the pinned version, the new name, this build's packages only, root's copies"
        );
        assert!(
            !dir.join("linux-cachyos-rt-7.2.9-1-x86_64.pkg.tar.zst")
                .exists(),
            "the old package is removed"
        );
    }

    fn log(m: &Machine, name: &str) -> String {
        std::fs::read_to_string(m.dir.join(name)).unwrap_or_default()
    }

    /// `kernel build linux-cachyos-bore-lto --config-fragment <fragment> --name <name>`
    fn build_args(name: &str, fragment: &Path) -> Vec<String> {
        [
            "kernel",
            "build",
            "linux-cachyos-bore-lto",
            "--config-fragment",
            fragment.to_str().unwrap(),
            "--name",
            name,
        ]
        .map(String::from)
        .to_vec()
    }

    fn strs(args: &[String]) -> Vec<&str> {
        args.iter().map(String::as_str).collect()
    }

    /// `kernel build` refuses an installed name, a bad fragment, a bad name and an unknown base before anything runs,
    /// and its plan names the pin, the package, the merge, the LTO, the install and the signed image.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_kernel_build_is_refused_or_planned_before_anything_runs() {
        let (m, _stand_ins, _asked, _processes, op) = build_machine("kplan").await;
        let fragment = m.dir.join("rt.config");
        let bad = m.dir.join("bad.config");
        std::fs::write(&bad, "PREEMPT_RT=y\n").unwrap();
        for (cmd, why) in [
            (
                build_args("bore-lto", &fragment),
                "linux-cachyos-bore-lto is an installed kernel already",
            ),
            (
                build_args("rt", &bad),
                "'PREEMPT_RT=y' is not a kernel option",
            ),
            (build_args("RT", &fragment), "the name must be 1-32 of a-z"),
            (
                ["kernel", "build", "linux-zen", "--name", "rt"]
                    .map(String::from)
                    .to_vec(),
                "no installed kernel linux-zen",
            ),
        ] {
            let refused = op.cli(&strs(&cmd));
            assert!(
                String::from_utf8_lossy(&refused.stderr).contains(why),
                "{why}: {refused:?}"
            );
        }
        let plan = op.ok(&[&strs(&build_args("rt", &fragment))[..], &["--dry-run"]].concat());
        for expected in [
            "linux-cachyos-bore/.SRCINFO is 7.2.9-2 (the installed linux-cachyos-bore-lto)",
            "the package linux-cachyos-rt (release 7.2.9-2-cachyos-rt); prepare() merges the fragment",
            "_use_llvm_lto=thin",
            "pacman -U (polkit org.legionctl.boot)",
            "the signed image legionctl-linux-cachyos-rt.efi",
        ] {
            assert!(plan.contains(expected), "{expected} missing:\n{plan}");
        }
        assert_eq!(
            (
                log(&m, "docker.log"),
                log(&m, "git.log"),
                log(&m, "pacman.log")
            ),
            Default::default(),
            "refusals and the dry run run nothing"
        );
        assert_eq!(last_job(&op, "kernel.build"), Value::Null);
    }

    /// A kernel build (todo-84 part 4) with the real operator and daemon: cut off by an operator restart in makepkg,
    /// it goes on as its job with the same clone, and packages of an earlier build in the folder (an older version,
    /// the same version with another fragment) are removed, never installed; the PKGBUILD of 7.2.9-2 (not the newer 7.2.10-1), the package named
    /// `linux-cachyos-<name>`, the fragment merged by `merge_config.sh` after the PKGBUILD's own `prepare()` and
    /// completed by `olddefconfig`, the base's LTO, then `pacman -U` of the kernel and its headers through the daemon
    /// (polkit). After pacman's hook, the new kernel has its signed image, and its name is refused as installed.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_kernel_is_built_from_its_pinned_pkgbuild_and_installed_through_the_daemon() {
        let (m, _stand_ins, asked, _processes, op) = build_machine("kbuild").await;
        let args = build_args("rt", &m.dir.join("rt.config"));
        std::fs::write(m.dir.join("hang"), "").unwrap();
        let _client = daemon_process::Kill::spawn(
            std::process::Command::new(env!("CARGO_BIN_EXE_legionctl"))
                .args(&args)
                .env_clear()
                .env("XDG_RUNTIME_DIR", &op.runtime)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()),
        );
        wait_for("makepkg", || m.dir.join("started").exists());
        assert_eq!(last_job(&op, "kernel.build")["state"], "running");
        let dir = m.dir.join(
            "home/.cache/legionctl/kernel-build/linux-cachyos-rt/linux-cachyos/linux-cachyos-bore",
        );
        stale_packages(&dir);
        drop(op);
        let op = on_bus(&m, "kbuild");
        op.ok(&["jobs", "list"]);
        wait_for("the end of the job", || {
            last_job(&op, "kernel.build")["state"] == "done"
        });
        assert_eq!(last_job(&op, "kernel.build")["attempts"], 2);
        assert_eq!(
            log(&m, "git.log"),
            "https://github.com/CachyOS/linux-cachyos\n",
            "one clone"
        );

        only_this_build_installed(&m, &dir);
        assert!(
            asked
                .lock()
                .unwrap()
                .iter()
                .any(|(_, action, _, _)| action == "org.legionctl.boot")
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("src/linux-7.2.9/localversion.20-pkgname")).unwrap(),
            "-cachyos-rt\n",
            "the release is 7.2.9-2-cachyos-rt"
        );
        assert_eq!(
            log(&m, "kconfig.log"),
            format!(
                "merge -m .config {}/legionctl.config\nCONFIG_PREEMPT_RT=y\n# CONFIG_DEBUG_INFO is not set\n\
                 make LLVM=1 olddefconfig\nmake -s kernelrelease\n",
                dir.display()
            ),
            "the fragment merged after the PKGBUILD's own configuration, then completed"
        );
        let runs: Vec<String> = log(&m, "docker.log")
            .lines()
            .filter(|l| l.starts_with("run "))
            .map(String::from)
            .collect();
        assert!(
            runs.len() == 2
                && runs[1].contains("-e _use_llvm_lto=thin")
                && runs[1].ends_with("makepkg -s -C --noconfirm"),
            "{runs:?}"
        );

        // pacman's hooks: mkinitcpio makes the initramfs, legionctl the signed image
        for (file, text) in [
            ("/usr/lib/modules/7.2.9-2-cachyos-rt/vmlinuz", ""),
            (
                "/usr/lib/modules/7.2.9-2-cachyos-rt/pkgbase",
                "linux-cachyos-rt",
            ),
            ("/boot/vmlinuz-linux-cachyos-rt", ""),
            ("/boot/initramfs-linux-cachyos-rt.img", ""),
        ] {
            m.set(file, text);
        }
        op.ok(&["boot", "rebuild"]);
        let image = std::fs::read_to_string(
            m.dir
                .join("root/boot/efi/EFI/Linux/legionctl-linux-cachyos-rt.efi"),
        )
        .unwrap();
        assert!(
            image.contains("vmlinuz-linux-cachyos-rt")
                && image.contains("--cmdline quiet rw root=UUID=a9e6 --")
                && image.contains("--secureboot-private-key"),
            "{image}"
        );
        let again = op.cli(&strs(&args));
        assert!(
            String::from_utf8_lossy(&again.stderr)
                .contains("linux-cachyos-rt is an installed kernel already"),
            "{again:?}"
        );
    }

    /// The recorded machine for the greeter: greetd, the greeter and GDM installed, GDM the display manager, greetd's
    /// configuration `original`, and a stand-in systemctl that moves the alias as systemd does; returns its log.
    fn greeter_machine(m: &Machine, original: &str) -> PathBuf {
        let root = m.dir.join("root");
        let units = "/usr/lib/systemd/system";
        for file in [
            "/usr/bin/greetd",
            "/usr/lib/legionctl/legionctl-greeter",
            "/usr/lib/systemd/system/gdm.service",
            "/usr/lib/systemd/system/greetd.service",
        ] {
            m.set(file, "");
        }
        std::fs::create_dir_all(root.join("etc/greetd")).unwrap();
        std::fs::write(root.join("etc/greetd/config.toml"), original).unwrap();
        let alias = root.join("etc/systemd/system/display-manager.service");
        std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(format!("{units}/gdm.service"), &alias).unwrap();
        let log = m.dir.join("systemctl.log");
        Machine::script(
            &m.dir.join("bin"),
            "systemctl",
            &format!(
                "echo \"$*\" >> {log}\n\
                 case \"$1\" in\n\
                 enable) /usr/bin/ln -sfn {units}/$2 {alias} ;;\n\
                 disable) [ \"$(/usr/bin/readlink {alias})\" = {units}/$2 ] && /usr/bin/rm {alias} ;;\n\
                 esac\nexit 0",
                log = log.display(),
                alias = alias.display()
            ),
        );
        // root's install makes the greeter's folder (todo-150); systemd-run logs the program, this its words
        let words = format!("echo \"$*\" >> {}", m.dir.join("units.log").display());
        Machine::script(&m.dir.join("bin"), "install", &words);
        log
    }

    /// The greeter in place of GDM (todo-83), through the operator and the daemon: refused, with nothing changed, while
    /// cage or greetd's user greeter is not there; then greetd's configuration is legionctl's (the old file kept byte for byte), GDM is
    /// disabled before greetd is enabled (the display-manager alias is one link); a second install cannot overwrite the
    /// kept file; remove puts the old file and GDM back. A stand-in systemctl moves the alias as systemd does.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_greeter_replaces_gdm_keeps_the_old_greetd_config_and_remove_puts_both_back() {
        let m = Machine::new("greeter");
        let root = m.dir.join("root");
        let original = "[terminal]\nvt = 1\n\n[default_session]\ncommand = \"agreety --cmd /bin/sh\"\nuser = \"greeter\"\n";
        let log = greeter_machine(&m, original);
        let units = m.dir.join("units.log");
        let (_stand_ins, asked, _processes) = start(&m).await;
        let op = on_bus(&m, "greeter");
        let config = || std::fs::read_to_string(root.join("etc/greetd/config.toml")).unwrap();
        let unit_changes = || -> Vec<String> {
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .filter(|l| l.starts_with("enable ") || l.starts_with("disable "))
                .map(String::from)
                .collect()
        };

        let refused = op.cli(&["greeter", "install"]);
        let why = String::from_utf8_lossy(&refused.stderr);
        assert!(
            why.contains("cage (pacman -S cage)")
                && why.contains("the system account greeter")
                && why.contains("pacman -S noto-fonts"),
            "{why}"
        );
        assert_eq!(config(), original);
        // the daemon's plan refuses before the polkit dialog
        assert_eq!((unit_changes(), asked.lock().unwrap().len()), (vec![], 0));

        m.set("/usr/bin/cage", "");
        m.set("/usr/share/fonts/noto/NotoSans-Regular.ttf", "");
        m.set(
            "/etc/passwd",
            "root:x:0:0::/root:/bin/bash\ngreeter:x:932:932::/:/bin/bash",
        );
        let users_list = r340_module_lists(&m);
        handover_files(&root, &op.ok(&["greeter", "install"]));
        // Health has the user's one step while legionctl's greeter is the login screen
        let doctor = String::from_utf8_lossy(&op.cli(&["doctor"]).stdout).into_owned();
        assert!(
            doctor.contains("sudo rm /etc/modules-load.d/nvidia.conf"),
            "{doctor}"
        );
        let ours = config();
        assert!(
            ours.starts_with("# legionctl greeter")
                && ours.contains(
                    "command = \"systemd-cat -t legionctl-greeter cage -s -m last -- env -u DISPLAY FONTCONFIG_FILE=/usr/share/legionctl/greeter/fonts.conf /usr/lib/legionctl/legionctl-greeter\""
                )
                && ours.contains("user = \"greeter\""),
            "{ours}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("etc/greetd/config.toml.before")).unwrap(),
            original
        );
        assert_eq!(
            unit_changes(),
            ["disable gdm.service", "enable greetd.service"]
        );
        let folder = root.join("var/lib/legionctl-greeter");
        assert_eq!(
            std::fs::read_to_string(&units).unwrap(),
            format!(
                "install\n-d -m 700 -o greeter -g greeter {}\n",
                folder.display()
            ),
            "the greeter's folder, in a transient unit outside the daemon's sandbox"
        );
        assert!(
            asked
                .lock()
                .unwrap()
                .iter()
                .any(|(_, action, _, _)| action == "org.legionctl.greeter")
        );
        let status = op.ok(&["greeter", "status"]);
        assert!(
            status.contains("login screen: legionctl's greeter")
                && status.contains("display manager at the next start: greetd.service"),
            "{status}"
        );

        let again = op.cli(&["greeter", "install"]);
        assert!(String::from_utf8_lossy(&again.stderr).contains("installed already"));
        assert_eq!(
            std::fs::read_to_string(root.join("etc/greetd/config.toml.before")).unwrap(),
            original,
            "the kept file is not overwritten with ours"
        );

        op.ok(&["greeter", "remove"]);
        assert_eq!(config(), original);
        assert!(!root.join("etc/greetd/config.toml.before").exists());
        handover_files_gone(&root, users_list);
        assert_eq!(
            unit_changes()[2..],
            ["disable greetd.service", "enable gdm.service"]
        );
        let status = op.ok(&["greeter", "status"]);
        assert!(
            status.contains("display manager at the next start: gdm.service"),
            "{status}"
        );
    }

    /// r340's module lists (boot b of 2026-10-17): nvidia-utils' list, the user's list of no package (returned), and
    /// another.
    fn r340_module_lists(m: &Machine) -> &'static str {
        // the legionctl package's two files, as make install puts them
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/greeter");
        for (from, to) in [
            ("greetd.service", "/etc/systemd/system/greetd.service"),
            (
                "nvidia-modules.conf",
                "/etc/modules-load.d/nvidia-580xx-utils.conf",
            ),
        ] {
            m.set(to, &std::fs::read_to_string(repo.join(from)).unwrap());
        }
        m.set(
            "/usr/lib/modules-load.d/nvidia-580xx-utils.conf",
            "nvidia-uvm",
        );
        m.set("/usr/lib/modules-load.d/ntsync.conf", "ntsync");
        let users_list = "nvidia\nnvidia_modeset\nnvidia-uvm\nnvidia_drm";
        m.set("/etc/modules-load.d/nvidia.conf", users_list);
        users_list
    }

    /// After remove: the package's two files have their content of the package again (greetd's own unit, the NVIDIA
    /// list); the user's list stays.
    fn handover_files_gone(root: &Path, users_list: &str) {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/greeter");
        for (package, path) in [
            ("greetd.service", "etc/systemd/system/greetd.service"),
            (
                "nvidia-modules.conf",
                "etc/modules-load.d/nvidia-580xx-utils.conf",
            ),
        ] {
            assert_eq!(
                std::fs::read_to_string(root.join(path)).unwrap(),
                std::fs::read_to_string(repo.join(package)).unwrap(),
                "{path}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(root.join("etc/modules-load.d/nvidia.conf"))
                .unwrap()
                .trim(),
            users_list
        );
    }

    /// legionctl's greeter files of boot-handover-2 (todo-200 item 1) after an install on r340's module lists: greetd's
    /// unit lets plymouth stay until the greeter, waits for udev's loads, and has no plymouth-quit-wait; the package's
    /// NVIDIA list is hidden by an empty one; another list stays; the user's own list is one step in `said`.
    fn handover_files(root: &Path, said: &str) {
        assert!(
            said.contains("sudo rm /etc/modules-load.d/nvidia.conf")
                && !said.contains("rm /etc/modules-load.d/nvidia-580xx-utils.conf"),
            "the one step for the list legionctl does not change, not for its own: {said}"
        );
        let unit = std::fs::read_to_string(root.join("etc/systemd/system/greetd.service")).unwrap();
        for line in [
            "Conflicts=getty@tty1.service plymouth-quit.service",
            "ExecStartPre=-/usr/bin/udevadm settle --timeout=10",
            "ExecStartPre=-/usr/bin/plymouth deactivate",
            "ExecStart=/usr/bin/greetd",
            "Alias=display-manager.service",
        ] {
            assert!(unit.lines().any(|l| l == line), "{line} in {unit}");
        }
        assert!(!unit.contains("plymouth-quit-wait"), "{unit}");
        let hidden =
            std::fs::read_to_string(root.join("etc/modules-load.d/nvidia-580xx-utils.conf"))
                .unwrap();
        assert!(
            hidden.starts_with("# legionctl greeter")
                && !hidden.lines().any(|l| l.starts_with("nvidia")),
            "{hidden}"
        );
        assert!(!root.join("etc/modules-load.d/ntsync.conf").exists());
    }

    /// r324's greeter (one window over all monitors, so the login sat at the edge of one; no folder for the last
    /// desktops): the package's upgrade step (`legionctl-system --greeter-update`, no question) makes greetd's
    /// configuration and the folder again, keeps the kept file and changes no unit (boot of 2026-10-17); with GDM as
    /// the login screen it changes nothing; after it, `greeter install` says it is installed already. The greeter of
    /// boot-handover reads only noto-fonts (its start parsed every system font: 3.5 s): without noto-fonts the old
    /// configuration stays, and the step says what to install.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_upgrade_gives_r324s_greeter_its_new_config_and_folder() {
        let m = Machine::new("greeterup");
        let root = m.dir.join("root");
        let r324 = "# legionctl greeter (legionctl greeter install; legionctl greeter remove puts /etc/greetd/config.toml.before back)\n[terminal]\nvt = 1\n\n[default_session]\ncommand = \"cage -s -- /usr/lib/legionctl/legionctl-greeter\"\nuser = \"greeter\"\n";
        let log = greeter_machine(&m, r324);
        m.set("/usr/bin/cage", "");
        m.set(
            "/etc/passwd",
            "root:x:0:0::/root:/bin/bash\ngreeter:x:932:932::/:/bin/bash",
        );
        m.set("/etc/greetd/config.toml.before", "kept");
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "greeterup");
        // the new greeter reads noto-fonts only: without it, the old configuration stays and the step says why
        let refused = one_shot(&m, &["--greeter-update"]);
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("pacman -S noto-fonts"),
            "{refused:?}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("etc/greetd/config.toml")).unwrap(),
            r324
        );
        m.set("/usr/share/fonts/noto/NotoSans-Regular.ttf", "");
        let updated = one_shot(&m, &["--greeter-update"]);
        assert!(
            updated.status.success(),
            "{}",
            String::from_utf8_lossy(&updated.stderr)
        );
        // one line for the upgrade's output; greetd's unit of legionctl; systemd reads it again
        assert!(
            String::from_utf8_lossy(&updated.stdout).contains(
                "login screen: the boot splash stays until it shows, and the NVIDIA driver loads beside the boot, not before it"
            ),
            "{updated:?}"
        );
        assert!(
            std::fs::read_to_string(root.join("etc/systemd/system/greetd.service"))
                .unwrap()
                .contains("ExecStartPre=-/usr/bin/plymouth deactivate")
        );
        assert!(
            std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .any(|l| l == "daemon-reload")
        );
        let ours = std::fs::read_to_string(root.join("etc/greetd/config.toml")).unwrap();
        assert!(
            ours.contains(
                "command = \"systemd-cat -t legionctl-greeter cage -s -m last -- env -u DISPLAY FONTCONFIG_FILE=/usr/share/legionctl/greeter/fonts.conf /usr/lib/legionctl/legionctl-greeter\""
            ),
            "{ours}"
        );
        assert!(
            std::fs::read_to_string(m.dir.join("units.log"))
                .unwrap()
                .contains(&root.join("var/lib/legionctl-greeter").display().to_string()),
            "the greeter's folder is made"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("etc/greetd/config.toml.before")).unwrap(),
            "kept\n"
        );
        let log = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            !log.lines()
                .any(|l| l.starts_with("enable ") || l.starts_with("disable ")),
            "no unit is enabled or disabled: {log}"
        );
        let again = op.cli(&["greeter", "install"]);
        assert!(String::from_utf8_lossy(&again.stderr).contains("installed already"));

        // GDM's (another) greetd configuration: the upgrade step leaves it as it is
        m.set(
            "/etc/greetd/config.toml",
            "[default_session]\ncommand = \"agreety\"",
        );
        let other = std::fs::read_to_string(root.join("etc/greetd/config.toml")).unwrap();
        let before = std::fs::read_to_string(m.dir.join("units.log")).unwrap();
        assert!(one_shot(&m, &["--greeter-update"]).status.success());
        assert_eq!(
            std::fs::read_to_string(root.join("etc/greetd/config.toml")).unwrap(),
            other
        );
        assert_eq!(
            std::fs::read_to_string(m.dir.join("units.log")).unwrap(),
            before
        );
    }

    /// `legionctl-system` in one of its one-shot modes (`--boot-rebuild`, `--remove-check`) on machine `m`, as a
    /// pacman hook runs it.
    fn one_shot(m: &Machine, args: &[&str]) -> std::process::Output {
        std::process::Command::new(crate::common::sibling("legionctl-system"))
            .args(args)
            .env_clear()
            .env("PATH", m.dir.join("bin"))
            .env("LEGIONCTL_SYSROOT", m.dir.join("root"))
            .output()
            .unwrap()
    }

    /// The pacman hook after a kernel upgrade (review 02, HIGH): the line of one kernel fails its check (a module
    /// that the kernel does not have); that kernel keeps its old image and the hook says why, in its output and in
    /// the journal, and `doctor` keeps the finding; the default kernel's image is made again all the same.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_pacman_hook_makes_each_kernel_image_on_its_own_and_reports_the_one_it_cannot_make()
    {
        let m = Machine::new("rebuildeach");
        boot_machine(&m);
        kernel_modules(&m);
        Machine::script(
            &m.dir.join("bin"),
            "logger",
            &format!("echo \"$*\" >> {}/journal.log", m.dir.display()),
        );
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "rebuildeach");
        op.ok(&["boot", "install"]);
        let linux = m.dir.join("root/boot/efi/EFI/Linux");
        let (other, default) = (
            linux.join("legionctl-linux-cachyos.efi"),
            linux.join("legionctl-linux-cachyos-bore-lto.efi"),
        );
        m.set(
            "/etc/legionctl/kernel/linux-cachyos.cmdline",
            "quiet rw zwsap.enabled=0 root=UUID=a9e6",
        );
        std::fs::write(&other, "old image").unwrap();
        std::fs::write(&default, "old image").unwrap();

        let hook = one_shot(&m, &["--boot-rebuild"]);
        let out = String::from_utf8_lossy(&hook.stdout);
        assert!(hook.status.success(), "{hook:?}");
        assert!(
            out.contains(
                "warning: the image of linux-cachyos is not made again, its old image stays"
            ) && out.contains("zwsap.enabled=0"),
            "{out}"
        );
        let made = std::fs::read_to_string(&default).unwrap();
        assert!(
            made.contains("vmlinuz-linux-cachyos-bore-lto")
                && made.contains("--secureboot-private-key"),
            "the default kernel's image is made again: {made}"
        );
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "old image");
        let journal = log(&m, "journal.log");
        assert!(
            journal.contains(
                "-t legionctl-system -p warning -- the image of linux-cachyos is not made again"
            ),
            "{journal}"
        );
        let doctor = String::from_utf8(op.cli(&["doctor"]).stdout).unwrap();
        assert!(
            doctor.contains(
                "boot rebuild (the pacman hook) does not make the signed image of linux-cachyos again"
            ) && !doctor.contains("image of linux-cachyos-bore-lto again"),
            "{doctor}"
        );
    }

    /// A parameter of a built-in module that `modules.builtin` does not list (review 02, MED: `rcupdate`, `rcutree`,
    /// `pcie_aspm`, `module`): the running kernel has the module in `/sys/module`, so its line takes it; another
    /// kernel's line takes it with a warning; a module that `/sys/module` does not have either is still refused.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_built_in_module_that_the_running_kernel_has_is_accepted_on_each_kernels_line() {
        let m = Machine::new("builtinparam");
        boot_machine(&m);
        kernel_modules(&m);
        m.set("/sys/module/rcupdate/parameters/rcu_expedited", "0");
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "builtinparam");
        op.ok(&["boot", "install"]);
        let line = "quiet rw root=UUID=a9e6 rcupdate.rcu_expedited=1";
        let trial = |kernel: &str| {
            std::fs::read_to_string(m.dir.join(format!(
                "root/boot/efi/EFI/Linux/legionctl-{kernel}.trial.efi"
            )))
            .unwrap_or_default()
        };

        let running = op.ok(&["kernel", "cmdline", "set", "linux-cachyos-bore-lto", line]);
        assert!(!running.contains("warning"), "{running}");
        assert!(trial("linux-cachyos-bore-lto").contains(&format!("--cmdline {line} --")));

        let other = op.ok(&["kernel", "cmdline", "set", "linux-cachyos", line]);
        assert!(
            other.contains(
                "warning: rcupdate.rcu_expedited=1: the running kernel has the module rcupdate (/sys/module)"
            ),
            "{other}"
        );
        assert!(trial("linux-cachyos").contains(&format!("--cmdline {line} --")));

        let refused = op.cli(&[
            "kernel",
            "cmdline",
            "set",
            "linux-cachyos",
            "quiet rw root=UUID=a9e6 rcutree.kthread_prio=1",
        ]);
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("has no module rcutree"),
            "{refused:?}"
        );
    }

    /// `kernel install` (review 02, MED): root runs `pacman -U` on files of the caller's folder. It takes only a
    /// kernel that `kernel build` makes (`linux-cachyos-<name>`), copies the packages into root's own folder, checks
    /// the copies' `.PKGINFO` (the name and base, one kernel package, one version), and pacman gets those copies only.
    #[tokio::test(flavor = "multi_thread")]
    async fn kernel_install_takes_only_a_built_kernel_and_installs_roots_checked_copies() {
        let m = Machine::new("kinstall");
        boot_machine(&m);
        pacman_stand_in(&m);
        let (stand_ins, _asked, _processes) = start(&m).await;
        let system = zbus::Proxy::new(
            &stand_ins,
            "org.legionctl.System1",
            "/org/legionctl/System1",
            "org.legionctl.System1",
        )
        .await
        .unwrap();
        let dir = m.dir.join("built");
        std::fs::create_dir_all(&dir).unwrap();
        let folder = dir.to_str().unwrap();
        let rt = "linux-cachyos-rt";
        let install = async |name: &str| -> String {
            match system
                .call::<_, _, Vec<String>>("Kernel", &("install", name, folder))
                .await
            {
                Ok(lines) => lines.join("\n"),
                Err(e) => format!("refused: {e}"),
            }
        };

        package(
            &dir,
            "glibc-2.41-1-x86_64.pkg.tar.zst",
            ["glibc", "glibc", "2.41-1", "x"],
        );
        let glibc = install("glibc").await;
        assert!(
            glibc.contains("refused: ")
                && glibc.contains("kernel install takes only a kernel of kernel build"),
            "{glibc}"
        );
        let posing = "linux-cachyos-rt-7.2.9-2-x86_64.pkg.tar.zst";
        package(&dir, posing, ["glibc", "glibc", "7.2.9-2", "x"]);
        let named = install(rt).await;
        assert!(
            named.contains("its .PKGINFO names glibc of glibc, not linux-cachyos-rt"),
            "{named}"
        );
        package(&dir, posing, [rt, rt, "7.2.9-2", "built"]);
        package(
            &dir,
            "linux-cachyos-rt-7.2.9-1-x86_64.pkg.tar.zst",
            [rt, rt, "7.2.9-1", "old"],
        );
        let two = install(rt).await;
        assert!(two.contains("2 packages of linux-cachyos-rt"), "{two}");
        assert_eq!(log(&m, "pacman.log"), "", "nothing reached pacman");

        std::fs::remove_file(dir.join("linux-cachyos-rt-7.2.9-1-x86_64.pkg.tar.zst")).unwrap();
        package(
            &dir,
            "linux-cachyos-rt-headers-7.2.9-1-x86_64.pkg.tar.zst",
            ["linux-cachyos-rt-headers", rt, "7.2.9-1", "old"],
        );
        let versions = install(rt).await;
        assert!(versions.contains("more than one version"), "{versions}");
        package(
            &dir,
            "linux-cachyos-rt-headers-7.2.9-1-x86_64.pkg.tar.zst",
            ["linux-cachyos-rt-headers", rt, "7.2.9-2", "built"],
        );
        let done = install(rt).await;
        assert!(!done.contains("refused"), "{done}");
        let stage = m.dir.join("root/var/lib/legionctl/kernel-install");
        assert_eq!(
            log(&m, "pacman.log"),
            format!(
                "{s}/linux-cachyos-rt-7.2.9-2-x86_64.pkg.tar.zst linux-cachyos-rt 7.2.9-2 built\n\
                 {s}/linux-cachyos-rt-headers-7.2.9-1-x86_64.pkg.tar.zst linux-cachyos-rt-headers 7.2.9-2 built\n",
                s = stage.display()
            ),
            "root's copies, never the caller's files"
        );
        assert_eq!(
            log(&m, "units.log"),
            "pacman\n",
            "pacman writes the system: a transient unit with no sandbox"
        );
        assert!(!stage.exists(), "the copies go after pacman");
    }

    /// The package's removal (review 02, MED): pacman ignores a failed `pre_remove`, so a hook before the
    /// transaction (`AbortOnFail`) stops the removal while the boot takeover is installed (its images would never be
    /// made again) or while legionctl's greeter cannot be taken back (GDM gone: greetd would start a deleted
    /// binary). The reason names the command to run first.
    #[test]
    fn the_package_removal_stops_while_the_boot_takeover_or_a_greeter_without_gdm_is_installed() {
        let m = Machine::new("removal");
        let efivars = boot_machine(&m);
        let hook = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../packaging/libalpm/05-legionctl-remove.hook"),
        )
        .unwrap();
        for line in [
            "Operation = Remove",
            "Target = legionctl",
            "When = PreTransaction",
            "AbortOnFail",
        ] {
            assert!(hook.lines().any(|l| l == line), "{line} missing:\n{hook}");
        }
        let exec: Vec<&str> = hook
            .lines()
            .find_map(|l| l.strip_prefix("Exec = "))
            .unwrap()
            .split_whitespace()
            .collect();
        assert_eq!(exec[0], "/usr/lib/legionctl/legionctl-system");
        let check = || one_shot(&m, &exec[1..]);
        let refusal = |out: std::process::Output| {
            assert!(!out.status.success(), "{out:?}");
            String::from_utf8(out.stderr).unwrap()
        };
        assert!(
            check().status.success(),
            "nothing of legionctl is installed"
        );

        let before = std::fs::read_to_string(&efivars).unwrap();
        let mut vars: Value = serde_json::from_str(&before).unwrap();
        vars["entries"]["0007"] = "legionctl".into();
        std::fs::write(&efivars, vars.to_string()).unwrap();
        let takeover = refusal(check());
        assert!(
            takeover.contains("the boot takeover is installed")
                && takeover.contains("Run `legionctl boot remove` first"),
            "{takeover}"
        );
        std::fs::write(&efivars, before).unwrap();

        m.set(
            "/etc/greetd/config.toml",
            "# legionctl greeter (legionctl greeter install)\n[default_session]\ncommand = \"cage -s -- /usr/lib/legionctl/legionctl-greeter\"",
        );
        let greeter = refusal(check());
        assert!(
            greeter.contains("GDM is not installed")
                && greeter.contains("`legionctl greeter remove`"),
            "{greeter}"
        );
        m.set("/usr/lib/systemd/system/gdm.service", "");
        assert!(check().status.success(), "pre_remove puts GDM back");
    }

    async fn greeter_act(
        system: &zbus::Proxy<'_>,
        user: &str,
        action: &str,
    ) -> zbus::Result<Vec<String>> {
        system.call("GreeterAct", &(user, action)).await
    }

    /// The greeter changes nothing that lasts (P-1, EV-review-08): a person at the login screen cannot turn on the
    /// Windows VM autostart of any account (a lasting polkit rule with no prompt); each user does it after login. A
    /// start before login still needs the user's own rule.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_greeter_cannot_write_a_lasting_autostart_rule() {
        let m = Machine::new("autostarts");
        m.set(
            "/etc/passwd",
            "root:x:0:0::/root:/bin/sh\nanna:x:1000:1000::/home/anna:/bin/sh\nben:x:1001:1001::/home/ben:/bin/sh",
        );
        let rules = m.dir.join("root/etc/polkit-1/rules.d");
        std::fs::create_dir_all(&rules).unwrap();
        let (stand_ins, _asked, _processes) = start(&m).await;
        let system = zbus::Proxy::new(
            &stand_ins,
            "org.legionctl.System1",
            "/org/legionctl/System1",
            "org.legionctl.System1",
        )
        .await
        .unwrap();
        for action in ["autostart-login", "autostart-boot", "autostart-off"] {
            let refused = greeter_act(&system, "anna", action).await.unwrap_err();
            assert!(
                refused
                    .to_string()
                    .contains("the greeter changes nothing that lasts"),
                "{refused}"
            );
        }
        assert_eq!(
            std::fs::read_dir(&rules).unwrap().count(),
            0,
            "no rule written"
        );
        let ben = greeter_act(&system, "ben", "vm-start").await.unwrap_err();
        assert!(
            ben.to_string()
                .contains("starts before login only with autostart on"),
            "{ben}"
        );
    }

    /// The battery (todo-82): `battery status` shows the charge mode in use (the bracketed charge type), the health
    /// (full vs design capacity), the cycles and USB charging while off; `battery mode` writes the charge type the
    /// battery offers and refuses one it does not; `usb-charging` writes ideapad's setting; the operator records a
    /// health sample when it starts (the daily trend).
    #[tokio::test(flavor = "multi_thread")]
    async fn the_battery_mode_health_and_usb_charging_follow_what_the_battery_offers() {
        let m = Machine::new("battery");
        let bat = "/sys/class/power_supply/BAT0";
        for (file, value) in [
            ("charge_types", "Fast Standard [Long_Life]"),
            ("energy_full", "95620000"),
            ("energy_full_design", "99900000"),
            ("cycle_count", "46"),
            ("capacity", "79"),
            ("status", "Not charging"),
        ] {
            m.set(&format!("{bat}/{file}"), value);
        }
        let usb = "/sys/bus/platform/drivers/ideapad_acpi/VPC2004:00/usb_charging";
        m.set(usb, "0");
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "battery");

        let status = op.ok(&["battery", "status"]);
        for expected in [
            "mode: conservation",
            "health: 95.7% (95.6 of 99.9 Wh), 46 cycles",
            "USB charging while off: off",
        ] {
            assert!(status.contains(expected), "{expected} missing:\n{status}");
        }
        op.ok(&["battery", "mode", "rapid"]);
        assert_eq!(m.file(&format!("{bat}/charge_types")), "Fast");
        op.ok(&["battery", "usb-charging", "on"]);
        assert_eq!(m.file(usb), "1");

        m.set(&format!("{bat}/charge_types"), "Standard [Long_Life]");
        let refused = op.cli(&["battery", "mode", "rapid"]);
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("this battery does not offer Fast"),
            "{}",
            String::from_utf8_lossy(&refused.stderr)
        );
        assert_eq!(
            m.file(&format!("{bat}/charge_types")),
            "Standard [Long_Life]",
            "nothing written"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let events = op.ok(&["events", "--json", "--limit", "1000"]);
            if events.contains("\"health_pct\": 95.7") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no battery sample: {events}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    type Props = std::collections::HashMap<String, zbus::zvariant::OwnedValue>;
    type Spec = (String, String, String, String);
    type MutterMode = (String, i32, i32, f64, f64, Vec<f64>, Props);
    type MutterState = (
        u32,
        Vec<(Spec, Vec<MutterMode>, Props)>,
        Vec<(i32, i32, f64, u32, bool, Vec<Spec>, Props)>,
        Props,
    );

    /// The MSI ultrawide as Mutter reports it (DP-3, 3440x1440, modes at 144 and 100 Hz) at `scale` and `hz`.
    fn mutter_state(scale: f64, hz: f64) -> MutterState {
        use zbus::zvariant::{OwnedValue, Value};
        let v = |x: Value<'_>| OwnedValue::try_from(x).unwrap();
        let spec: Spec = ("DP-3".into(), "MSI".into(), "MAG341".into(), "0".into());
        let mode = |rate: f64| -> MutterMode {
            let props = Props::from([
                (
                    "is-current".to_string(),
                    v(Value::from((rate - hz).abs() < 0.5)),
                ),
                ("refresh-rate-mode".to_string(), v(Value::from("fixed"))),
            ]);
            (
                format!("3440x1440@{rate}"),
                3440,
                1440,
                rate,
                1.0,
                vec![1.0, 1.25],
                props,
            )
        };
        let monitor_props = Props::from([
            ("display-name".to_string(), v(Value::from("Microstep 34\""))),
            ("is-builtin".to_string(), v(Value::from(false))),
        ]);
        (
            1,
            vec![(spec.clone(), vec![mode(144.0), mode(100.0)], monitor_props)],
            vec![(0, 0, scale, 0, true, vec![spec], Props::new())],
            Props::new(),
        )
    }

    /// What the stand-in Mutter shows (scale, refresh rate of DP-3), and the method of each `ApplyMonitorsConfig`.
    type Shown = std::sync::Arc<std::sync::Mutex<(f64, f64)>>;
    type Methods = std::sync::Arc<std::sync::Mutex<Vec<u32>>>;

    /// A desktop session on the machine's private bus: a stand-in Mutter (`GetCurrentState` from `shown`;
    /// `ApplyMonitorsConfig` sets DP-3's rate from the mode id and logs the method), the MSI's EDID (80 x 34 cm),
    /// VS Code and Thorium installed, a stand-in gsettings that keeps its values in `<dir>/dconf`, and an operator
    /// with a temp HOME.
    async fn desktop(
        m: &Machine,
        tag: &str,
        shown: Shown,
        methods: Methods,
    ) -> (zbus::Connection, Operator) {
        type Member = (String, String, Props);
        type Logical = (i32, i32, f64, u32, bool, Vec<Member>);
        let mut edid = vec![0u8; 128];
        (edid[21], edid[22]) = (80, 34);
        let edid_path = m.dir.join("root/sys/class/drm/card0-DP-3/edid");
        std::fs::create_dir_all(edid_path.parent().unwrap()).unwrap();
        std::fs::write(edid_path, edid).unwrap();
        m.set("/usr/bin/code", "");
        m.set("/usr/bin/thorium-browser", "");
        let dconf = m.dir.join("dconf");
        std::fs::create_dir_all(&dconf).unwrap();
        Machine::script(
            &m.dir.join("bin"),
            "gsettings",
            &format!(
                "case $1 in get) read -r v < '{0}'/\"$3\" && printf '%s\\n' \"$v\" ;; set) printf '%s\\n' \"$4\" > '{0}'/\"$3\" ;; esac",
                dconf.display()
            ),
        );
        let address = format!("unix:path={}", m.dir.join("bus").display());
        let mutter = zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .name("org.gnome.Mutter.DisplayConfig")
            .unwrap()
            .build()
            .await
            .unwrap();
        let replies = mutter.clone();
        tokio::spawn(async move {
            let mut calls = zbus::MessageStream::from(&replies);
            while let Some(Ok(call)) = futures_util::StreamExt::next(&mut calls).await {
                let member = call.header().member().map(ToString::to_string);
                let reply = zbus::message::Message::method_return(&call.header()).unwrap();
                let reply = match member.as_deref() {
                    Some("GetCurrentState") => {
                        let (scale, hz) = *shown.lock().unwrap();
                        reply.build(&mutter_state(scale, hz)).unwrap()
                    }
                    Some("ApplyMonitorsConfig") => {
                        let (_, method, logical, _): (u32, u32, Vec<Logical>, Props) =
                            call.body().deserialize().unwrap();
                        let mode = &logical[0].5[0].1;
                        let hz = mode.split_once('@').unwrap().1.parse().unwrap();
                        shown.lock().unwrap().1 = hz;
                        methods.lock().unwrap().push(method);
                        reply.build(&()).unwrap()
                    }
                    _ => continue,
                };
                replies.send(&reply).await.unwrap();
            }
        });
        let op = Operator::start(
            tag,
            &[
                ("PATH", m.dir.join("bin").into()),
                ("HOME", m.dir.join("home").into()),
                ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
                ("DBUS_SYSTEM_BUS_ADDRESS", address.clone().into()),
                ("DBUS_SESSION_BUS_ADDRESS", address.into()),
            ],
        );
        (mutter, op)
    }

    /// Monitors and fonts (todo-81 part 2) through a stand-in Mutter on the session bus: `display status` and `doctor`
    /// name an external monitor below its highest refresh rate and grayscale antialiasing where subpixel is sharper
    /// (integer scale, normal density from the EDID); an Electron app through `XWayland` only at a fractional scale
    /// (at integer scales Chromium on Wayland draws grayscale text, which was worse here: user report 2026-10-14).
    #[tokio::test(flavor = "multi_thread")]
    async fn monitors_and_fonts_are_checked_against_the_scale_and_density_in_use() {
        let m = Machine::new("display");
        let (_stand_ins, _asked, (_bus, _daemon)) = start(&m).await;
        let state = Shown::new(std::sync::Mutex::new((1.0, 100.0)));
        let (_mutter, op) = desktop(&m, "display", state.clone(), Methods::default()).await;
        std::fs::write(m.dir.join("dconf/font-antialiasing"), "'grayscale'\n").unwrap();
        let shown = op.ok(&["display", "status"]);
        for expected in [
            "DP-3 Microstep 34\": 3440x1440 at 100 Hz (up to 144), scale 1, 109 PPI",
            "fonts: grayscale antialiasing",
            "- [refresh-DP-3] Microstep 34\" (DP-3) runs at 100 Hz, and 144 Hz is available at 3440x1440",
            "- [antialiasing] fonts use grayscale antialiasing, and every monitor has an integer scale at normal \
             density, where subpixel antialiasing (RGB order) is sharper",
        ] {
            assert!(shown.contains(expected), "{expected} missing:\n{shown}");
        }
        assert!(
            !shown.contains("XWayland"),
            "no XWayland finding at an integer scale:\n{shown}"
        );
        // findings make doctor exit 1
        let doctor = String::from_utf8_lossy(&op.cli(&["doctor"]).stdout).to_string();
        assert!(
            doctor.contains("(fix: legionctl display fix refresh-DP-3)"),
            "{doctor}"
        );

        *state.lock().unwrap() = (1.25, 144.0);
        let fractional = op.ok(&["display", "status"]);
        assert!(
            fractional.contains(
                "[xwayland-code] /usr/bin/code draws through XWayland, blurry at this fractional scale"
            ),
            "{fractional}"
        );
        assert!(
            !fractional.contains("subpixel") && !fractional.contains("Hz is available"),
            "{fractional}"
        );
    }

    /// `display fix` with `args`: the event that `undo` takes.
    fn fixed(op: &Operator, args: &[&str]) -> String {
        let done = op.ok(&[["display", "fix"].as_slice(), args].concat());
        done.split_once("legionctl undo ")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .unwrap_or_else(|| panic!("no undo event:\n{done}"))
            .to_string()
    }

    /// The findings among `ids` that doctor names now.
    fn found<'a>(op: &Operator, ids: &[&'a str]) -> Vec<&'a str> {
        let doctor = String::from_utf8_lossy(&op.cli(&["doctor"]).stdout).to_string();
        ids.iter()
            .copied()
            .filter(|id| doctor.contains(&format!("(fix: legionctl display fix {id})")))
            .collect()
    }

    /// The fixes of the user's own settings and files as operations (todo-81 part 3): `display fix all --dry-run`
    /// changes nothing; `display fix all` changes each value (the fonts' antialiasing, a flags file, a monitor's mode,
    /// the session's environment), and its findings go; `undo` gives back each exact old value (the dconf text, the
    /// file's bytes or its absence, the mode, with Mutter's method of the fix); a system change is refused. Both
    /// directions of the flags file: the Wayland line comes out at an integer scale and goes in at a fractional one.
    #[tokio::test(flavor = "multi_thread")]
    async fn display_fixes_change_your_settings_and_undo_gives_back_the_exact_old_values() {
        let m = Machine::new("displayfix");
        let (_stand_ins, _asked, (_bus, _daemon)) = start(&m).await;
        let state = Shown::new(std::sync::Mutex::new((1.0, 100.0)));
        let methods = Methods::default();
        let (_mutter, op) = desktop(&m, "displayfix", state.clone(), methods.clone()).await;
        let dconf = |key: &str| std::fs::read_to_string(m.dir.join("dconf").join(key)).unwrap();
        std::fs::write(m.dir.join("dconf/font-antialiasing"), "'grayscale'\n").unwrap();
        std::fs::write(m.dir.join("dconf/font-rgba-order"), "'bgr'\n").unwrap();
        let config = m.dir.join("home/.config");
        std::fs::create_dir_all(&config).unwrap();
        // no newline at the end: the bytes must come back as they were
        let thorium_flags = "--enable-features=X\n--ozone-platform=wayland\n--force-dark-mode";
        std::fs::write(config.join("thorium-flags.conf"), thorium_flags).unwrap();
        std::fs::write(config.join("code-flags.conf"), "--disable-gpu").unwrap();
        let read = |name: &str| std::fs::read_to_string(config.join(name)).ok();
        let opencl = "environment.d/legionctl-opencl.conf";
        let fixable = [
            "refresh-DP-3",
            "antialiasing",
            "wayland-thorium",
            "opencl-iris",
        ];

        let planned = op.ok(&["display", "fix", "all", "--dry-run"]);
        for expected in [
            "org.gnome.desktop.interface font-antialiasing: 'rgba' (was 'grayscale')".to_string(),
            "org.gnome.desktop.interface font-rgba-order: 'rgb' (was 'bgr')".into(),
            "DP-3 mode: 3440x1440@144 (was 3440x1440@100)".into(),
            format!(
                "{}: \"--enable-features=X\\n--force-dark-mode\" (was {thorium_flags:?})",
                config.join("thorium-flags.conf").display()
            ),
            format!(
                "{}: \"RUSTICL_ENABLE=iris\\n\" (was no file)",
                config.join(opencl).display()
            ),
            "not changed, a system change: [switcheroo]".into(),
        ] {
            assert!(
                planned.contains(&expected),
                "{expected} missing:\n{planned}"
            );
        }
        assert!(!planned.contains("legionctl undo"), "{planned}");
        assert_eq!(
            (dconf("font-antialiasing"), dconf("font-rgba-order")),
            ("'grayscale'\n".into(), "'bgr'\n".into())
        );
        assert_eq!(read("thorium-flags.conf").as_deref(), Some(thorium_flags));
        assert_eq!(read(opencl), None);
        assert!(
            methods.lock().unwrap().is_empty(),
            "the dry run applied a mode"
        );

        let refused = op.cli(&["display", "fix", "switcheroo"]);
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(
            !refused.status.success() && stderr.contains("switcheroo is a system change"),
            "{stderr}"
        );

        let event = fixed(&op, &["all"]);
        assert_eq!(
            (dconf("font-antialiasing"), dconf("font-rgba-order")),
            ("'rgba'\n".into(), "'rgb'\n".into())
        );
        assert_eq!(
            read("thorium-flags.conf").as_deref(),
            Some("--enable-features=X\n--force-dark-mode")
        );
        assert_eq!(read(opencl).as_deref(), Some("RUSTICL_ENABLE=iris\n"));
        assert_eq!(state.lock().unwrap().1, 144.0);
        assert_eq!(found(&op, &fixable), Vec::<&str>::new());
        // the system change stays a finding, with no fix
        let doctor = String::from_utf8_lossy(&op.cli(&["doctor"]).stdout).to_string();
        let switcheroo = "switcheroo-control does not run: GNOME has no \"Launch using Discrete Graphics Card\"\n";
        assert!(doctor.contains(switcheroo), "{doctor}");

        op.ok(&["undo", &event]);
        assert_eq!(
            (dconf("font-antialiasing"), dconf("font-rgba-order")),
            ("'grayscale'\n".into(), "'bgr'\n".into())
        );
        assert_eq!(read("thorium-flags.conf").as_deref(), Some(thorium_flags));
        assert_eq!(read(opencl), None);
        assert_eq!(state.lock().unwrap().1, 100.0);
        assert_eq!(*methods.lock().unwrap(), [1, 1], "temporary, as the fix");
        assert_eq!(found(&op, &fixable), fixable);

        // at a fractional scale the Wayland line goes in, and a persistent mode is undone persistently
        *state.lock().unwrap() = (1.25, 100.0);
        let event = fixed(&op, &["all", "--persistent"]);
        assert_eq!(
            read("code-flags.conf").as_deref(),
            Some("--disable-gpu\n--ozone-platform-hint=auto\n")
        );
        assert_eq!(read("thorium-flags.conf").as_deref(), Some(thorium_flags));
        op.ok(&["undo", &event]);
        assert_eq!(read("code-flags.conf").as_deref(), Some("--disable-gpu"));
        assert_eq!(*methods.lock().unwrap(), [1, 1, 2, 2]);

        let unknown = op.cli(&["display", "fix", "refresh-HDMI-1"]);
        assert!(
            String::from_utf8_lossy(&unknown.stderr).contains("no finding refresh-HDMI-1 now"),
            "{}",
            String::from_utf8_lossy(&unknown.stderr)
        );
    }

    /// The Windows VM's full start, as root (only root may raise the memory lock this far): QEMU runs on the
    /// configured CPUs with the Windows disk through VFIO, the Bolt receiver, the firmware's MSDM table, a TPM and a
    /// free `PCIe` port for a GPU; when it ends, the host is given back.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs root: CI runs it with sudo"]
    async fn as_root_the_windows_vm_starts_with_its_disk_and_gives_the_host_back() {
        let m = Machine::new("winvmroot");
        let _kernel = windows_machine(&m);
        m.set("/proc/1/mounts", "/dev/nvme1n1p2 / btrfs rw 0 0");
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "winvmroot");
        let out = windows_runner(&m, &op);
        assert!(out.status.success(), "{}", windows_state(&m));
        assert_eq!(windows_state(&m), "stopped");
        let args = std::fs::read_to_string(m.dir.join("qemu.args")).unwrap();
        let dir = m.dir.join("home/.local/share/legionctl/windows");
        for expected in [
            "vfio-pci,host=0000:06:00.0".to_string(),
            "usb-host,bus=xhci.0,vendorid=0x046d,productid=0xc548".into(),
            format!("file={}", dir.join("msdm.bin").display()),
            "pcie-root-port,id=gpu-port,chassis=1,slot=1".into(),
            "tpm-crb,tpmdev=tpm0".into(),
            "64M".into(),
            "q35,accel=kvm,smm=off,memory-backend=mem".into(),
            "memory-backend-memfd,id=mem,size=64M,share=on".into(),
            "vhost-user-fs-pci,chardev=fs0,tag=share".into(),
            "ivshmem-plain,memdev=lg".into(),
        ] {
            assert!(
                args.lines().any(|l| l == expected),
                "{expected} missing:\n{args}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("msdm.bin"))
                .unwrap()
                .trim(),
            "MSDM-license"
        );
        assert_eq!(
            std::fs::read_to_string(m.dir.join("taskset.cpus"))
                .unwrap()
                .trim(),
            "0-7,16-19"
        );
        let driver =
            std::fs::read_link(m.dir.join("root/sys/bus/pci/devices/0000:06:00.0/driver")).unwrap();
        assert_eq!(driver, Path::new("../../drivers/nvme"), "the disk is back");
        assert!(
            op.ok(&["windows", "vm", "stop"])
                .contains("no Windows VM ran")
        );
    }

    /// The D-Bus contract is reviewed: the running daemon's interface is `dbus/org.legionctl.System1.xml`, word for
    /// word (`LEGIONCTL_WRITE_DBUS=1` writes it after a deliberate change), and its `Version` is that file's hash,
    /// which the operator's doctor compares with its own.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_daemon_serves_the_committed_d_bus_interface() {
        let m = Machine::new("dbusxml");
        let (stand_ins, _asked, _processes) = start(&m).await;
        let xml: String = zbus::Proxy::new(
            &stand_ins,
            "org.legionctl.System1",
            "/org/legionctl/System1",
            "org.freedesktop.DBus.Introspectable",
        )
        .await
        .unwrap()
        .call("Introspect", &())
        .await
        .unwrap();
        let start = xml
            .find("<interface name=\"org.legionctl.System1\">")
            .expect("the interface");
        let end = xml[start..].find("</interface>").unwrap() + start + "</interface>".len();
        let ours = format!(
            "<!DOCTYPE node PUBLIC \"-//freedesktop//DTD D-BUS Object Introspection 1.0//EN\"\n \
             \"http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd\">\n<node>\n  {}\n</node>\n",
            &xml[start..end]
        );
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dbus/org.legionctl.System1.xml");
        if std::env::var_os("LEGIONCTL_WRITE_DBUS").is_some() {
            std::fs::write(&path, &ours).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            ours,
            "the daemon's interface differs from dbus/org.legionctl.System1.xml; if meant, run \
             LEGIONCTL_WRITE_DBUS=1 cargo test -p legionctl --test machine the_daemon_serves and rebuild"
        );
        let version: String = zbus::Proxy::new(
            &stand_ins,
            "org.legionctl.System1",
            "/org/legionctl/System1",
            "org.legionctl.System1",
        )
        .await
        .unwrap()
        .get_property("Version")
        .await
        .unwrap();
        assert_eq!(version, legionctl_machine::interface_version());
    }

    /// The operator of the idle-freeze test: a stand-in systemctl that keeps the scope's freezer state in a file and
    /// logs freeze and thaw, a repository, and `session.policy.idle_freeze_minutes` 1.
    fn idle_operator(
        m: &Machine,
        address: String,
    ) -> (Operator, std::path::PathBuf, std::path::PathBuf) {
        let bin = m.dir.join("bin");
        let scope_file = m.dir.join("freezer");
        std::fs::write(&scope_file, "running").unwrap();
        Machine::script(
            &bin,
            "systemctl",
            &format!(
                "f={f}; case \"$2\" in is-active) echo active;; show) cat \"$f\";; freeze) echo frozen > \"$f\"; \
                 echo \"freeze $3\" >> {log};; thaw) echo running > \"$f\"; echo \"thaw $3\" >> {log};; esac",
                f = scope_file.display(),
                log = m.dir.join("systemctl.log").display()
            ),
        );
        let home = m.dir.join("home");
        let repo = home.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            home.join(".gitconfig"),
            "[user]\n  name = Test\n  email = test@example.com\n",
        )
        .unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "x",
            ],
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&repo)
                    .env("HOME", &home)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        std::fs::create_dir_all(home.join(".config/legionctl")).unwrap();
        std::fs::write(
            home.join(".config/legionctl/idle.jsonc"),
            serde_json::json!({
                "session": {"agents": {"probe": ["true"]}, "writable": [], "policy": {"idle_freeze_minutes": 1}},
                "workspace": {"root": home.join("worktrees")}
            })
            .to_string(),
        )
        .unwrap();
        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let op = Operator::start(
            "idle",
            &[
                ("PATH", path),
                ("HOME", home.clone().into()),
                ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
                ("DBUS_SYSTEM_BUS_ADDRESS", address.clone().into()),
                ("DBUS_SESSION_BUS_ADDRESS", address.into()),
            ],
        );
        (op, home, repo)
    }

    /// Idle freeze (S4 policy, decided 2026-10-13): with `session.policy.idle_freeze_minutes` set, a running
    /// session whose agent ended a reply and got no prompt for that long is frozen once: the user gets a notice with
    /// the command that thaws it, `session list` says frozen, and after `session unpause` it is not frozen again
    /// until a new reply ends. Stand-ins: systemctl (scope state in a file) and the desktop on the private bus.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_idle_session_is_frozen_once_and_the_user_hears_how_to_thaw_it() {
        let m = Machine::new("idle");
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let address = format!("unix:path={}", m.dir.join("bus").display());
        let (_desktop, told) = crate::desktop::answering(&address, &["ok", "ok", "ok"]).await;
        let (op, home, repo) = idle_operator(&m, address);
        let started = op.run(
            &repo,
            &[("HOME", home.to_str().unwrap())],
            &["session", "start", "probe", "--dossier", "idle", "--json"],
            "",
        );
        assert!(
            started.status.success(),
            "{}",
            String::from_utf8_lossy(&started.stderr)
        );
        let id = serde_json::from_slice::<Value>(&started.stdout).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        // the agent ended a reply two minutes ago, and no prompt came
        op.run(
            &repo,
            &[("LEGIONCTL_SESSION", id.as_str())],
            &["hook", "stop"],
            &serde_json::json!({"session_id": id, "cwd": repo, "hook_event_name": "Stop"})
                .to_string(),
        );
        let aged = std::process::Command::new("python3")
            .args(["-c", "import sqlite3, sys; c = sqlite3.connect(sys.argv[1]); \
                    c.execute(\"UPDATE events SET ts = ts - 120 WHERE kind = 'internal.hooks.run'\"); c.commit()"])
            .arg(home.join(".local/state/legionctl/operator.db"))
            .status()
            .unwrap();
        assert!(aged.success());
        let log = m.dir.join("systemctl.log");
        let frozen_times = || {
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .matches("freeze")
                .count()
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
        while frozen_times() == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the idle session was not frozen"
            );
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        let list: Value = serde_json::from_str(&op.ok(&["session", "list", "--json"])).unwrap();
        assert_eq!(list[0]["state"], "frozen", "{list}");
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let notices = told.lock().unwrap().clone();
        assert!(
            notices
                .iter()
                .any(|b| b.contains(&format!("legionctl session unpause {id}"))),
            "{notices:?}"
        );
        op.ok(&["session", "unpause", &id]);
        // two more checks pass: the old reply does not freeze it again
        tokio::time::sleep(std::time::Duration::from_secs(21)).await;
        assert_eq!(
            frozen_times(),
            1,
            "{}",
            std::fs::read_to_string(&log).unwrap()
        );
    }

    /// A repository's hooks file: a command bound to every prompt.
    fn hooks_layer(repo: &std::path::Path, command: &str) {
        std::fs::create_dir_all(repo.join(".legionctl")).unwrap();
        std::fs::write(
            repo.join(".legionctl/hooks.jsonc"),
            serde_json::json!({"hooks": {
                "actions": {"probe": {"command": command, "timeout": 5}},
                "bindings": {"prompt": ["probe"]}
            }})
            .to_string(),
        )
        .unwrap();
    }

    /// The operator of `a_repository_sets_a_powerful_key_only_after_the_user_allows_its_content` on the private bus
    /// (also its session bus, for the desktop), and its repository with a hooks file.
    fn tofu_operator(
        m: &Machine,
        (tag, address): (&str, String),
    ) -> (Operator, std::path::PathBuf, std::path::PathBuf) {
        let home = m.dir.join("home");
        let repo = home.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        hooks_layer(&repo, "echo PROBE_FIRST");
        let path = std::env::join_paths(
            std::iter::once(m.dir.join("bin"))
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let op = Operator::start(
            tag,
            &[
                ("PATH", path),
                ("HOME", home.clone().into()),
                ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
                ("DBUS_SYSTEM_BUS_ADDRESS", address.clone().into()),
                ("DBUS_SESSION_BUS_ADDRESS", address.into()),
            ],
        );
        (op, home, repo)
    }

    /// Trust on first use (O-2, user decision 2026-10-16): a repository's `.legionctl` file that sets a powerful key
    /// (here a hook command on every prompt) is not used for it until the user allows that content once, through the
    /// desktop question; the next prompts run it without a question. A changed file asks again, and after Deny its
    /// command does not run and nobody is asked again for that content. A repository's checks (shell commands that
    /// the user's checks and merge gates run on the host, A-5) wait for an answer too: until then they do not
    /// resolve, so no check runs and no gate passes.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_repository_sets_a_powerful_key_only_after_the_user_allows_its_content() {
        let m = Machine::new("tofu");
        let (_stand_ins, _asked, (_bus, _daemon)) = start(&m).await;
        let address = format!("unix:path={}", m.dir.join("bus").display());
        let (_desktop, asked) =
            crate::desktop::answering(&address, &["allow", "deny", "deny"]).await;
        let (op, home, repo) = tofu_operator(&m, (m.tag, address));
        let layer = |command: &str| hooks_layer(&repo, command);
        let env = [
            ("LEGIONCTL_SESSION", "s1"),
            ("HOME", home.to_str().unwrap()),
        ];
        assert!(
            op.run(&repo, &env, &["dossier", "use", "tofu"], "")
                .status
                .success()
        );
        let prompt = || {
            let out = op.run(
                &repo,
                &env,
                &["hook", "prompt"],
                &serde_json::json!({"session_id": "s1", "cwd": repo, "hook_event_name": "UserPromptSubmit",
                                    "prompt": "hi"})
                .to_string(),
            );
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        let approvals = home.join(".local/state/legionctl/approvals.json");
        let answered = |n: usize| {
            for _ in 0..200 {
                let text = std::fs::read_to_string(&approvals).unwrap_or_default();
                if text.matches("hooks.jsonc").count() >= 1 && asked.lock().unwrap().len() >= n {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            panic!("no answer stored: {:?}", asked.lock().unwrap());
        };

        let first = prompt();
        assert!(
            !first.contains("PROBE_FIRST"),
            "used before the user allowed it: {first}"
        );
        answered(1);
        let allowed = prompt();
        assert!(allowed.contains("PROBE_FIRST"), "{allowed}");
        assert!(prompt().contains("PROBE_FIRST"));
        assert_eq!(
            asked.lock().unwrap().len(),
            1,
            "one question for one content"
        );
        let question = asked.lock().unwrap()[0].clone();
        assert!(
            question.contains("hooks.jsonc") && question.contains("hooks"),
            "{question}"
        );

        layer("echo PROBE_CHANGED");
        let changed = prompt();
        assert!(
            !changed.contains("PROBE_CHANGED") && !changed.contains("PROBE_FIRST"),
            "{changed}"
        );
        for _ in 0..200 {
            if std::fs::read_to_string(&approvals)
                .unwrap_or_default()
                .contains("false")
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            !prompt().contains("PROBE_CHANGED"),
            "denied content stays unused"
        );
        assert_eq!(
            asked.lock().unwrap().len(),
            2,
            "a changed file asks again, a denied content does not"
        );
        a_repositorys_check_waits(&op, (&home, &repo), &env, &asked);
    }

    /// A-5: a repository's check (a shell command on the host for the user) does not run until the user allows its
    /// file; the attempt asks (the third question of `asked`).
    fn a_repositorys_check_waits(
        op: &Operator,
        (home, repo): (&std::path::Path, &std::path::Path),
        env: &[(&str, &str)],
        asked: &std::sync::Mutex<Vec<String>>,
    ) {
        std::fs::write(
            repo.join(".legionctl/checks.jsonc"),
            r#"{"checks": {"lint": {"kind": "lint", "run": "touch PWNED"}}}"#,
        )
        .unwrap();
        let checks = op.run(repo, env, &["code", "checks", "run"], "");
        assert!(
            !checks.status.success()
                && String::from_utf8_lossy(&checks.stderr)
                    .contains("not used until you allow this file"),
            "{checks:?}"
        );
        assert!(
            !repo.join("PWNED").exists(),
            "a repository's check ran before the user allowed it"
        );
        answered(home, "checks.jsonc", asked, 3);
        assert!(
            asked.lock().unwrap()[2].contains("checks.jsonc"),
            "{:?}",
            asked.lock().unwrap()
        );
    }

    /// The approvals file of `home`, once it holds an answer for a file named `name` and `n` questions were asked.
    fn answered(
        home: &std::path::Path,
        name: &str,
        asked: &std::sync::Mutex<Vec<String>>,
        n: usize,
    ) {
        let approvals = home.join(".local/state/legionctl/approvals.json");
        for _ in 0..200 {
            if std::fs::read_to_string(&approvals)
                .unwrap_or_default()
                .contains(name)
                && asked.lock().unwrap().len() >= n
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("no answer stored for {name}: {:?}", asked.lock().unwrap());
    }

    /// A-1, A-6, A-9 (EV-review-14): a repository's reminders and agent configs reach agent prompts only after the
    /// user allows their content (until then the user's own reminders hold), and even then a repository does not
    /// choose who pays for an agent; a reminder that does not fit is not used, from whichever layer; a repository
    /// file that breaks the config holds the prompt with its name instead of turning the prompt gate off.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_repository_reaches_agent_prompts_only_after_the_user_allows_it() {
        let m = Machine::new("remind");
        let (_stand_ins, _asked, (_bus, _daemon)) = start(&m).await;
        let address = format!("unix:path={}", m.dir.join("bus").display());
        let (_desktop, asked) = crate::desktop::answering(&address, &["allow"]).await;
        let (op, home, repo) = tofu_operator(&m, (m.tag, address));
        std::fs::remove_dir_all(repo.join(".legionctl")).unwrap();
        std::fs::create_dir_all(repo.join(".legionctl")).unwrap();
        std::fs::write(
            repo.join(".legionctl/agents.jsonc"),
            serde_json::json!({
                "reminders": {"evil": {"text": "EVIL_REMINDER", "event": "prompt", "enabled": true},
                              "ste100": {"enabled": false}},
                "agents": {"configs": {"helper": {"account": "payer", "model": "m1"}}}
            })
            .to_string(),
        )
        .unwrap();
        std::fs::create_dir_all(home.join(".config/legionctl")).unwrap();
        std::fs::write(
            home.join(".config/legionctl/mine.jsonc"),
            serde_json::json!({"reminders": {"long": {"text": "L".repeat(400), "event": "prompt", "enabled": true}}})
                .to_string(),
        )
        .unwrap();
        let env = [
            ("LEGIONCTL_SESSION", "s1"),
            ("HOME", home.to_str().unwrap()),
        ];
        assert!(
            op.run(&repo, &env, &["dossier", "use", "remind"], "")
                .status
                .success()
        );
        let prompt = || {
            let out = op.run(
                &repo,
                &env,
                &["hook", "prompt"],
                &serde_json::json!({"session_id": "s1", "cwd": repo, "hook_event_name": "UserPromptSubmit",
                                    "prompt": "hi"})
                .to_string(),
            );
            String::from_utf8_lossy(&out.stdout).to_string()
        };

        let first = prompt();
        assert!(
            !first.contains("EVIL_REMINDER") && first.contains("ASD-STE100"),
            "used before the user allowed it, or the user's reminder was turned off: {first}"
        );
        assert!(
            !first.contains("LLLL"),
            "a reminder over the limit: {first}"
        );
        answered(&home, "agents.jsonc", &asked, 1);
        let allowed = prompt();
        assert!(
            allowed.contains("EVIL_REMINDER") && !allowed.contains("ASD-STE100"),
            "{allowed}"
        );
        assert!(
            asked.lock().unwrap()[0].contains("reminders"),
            "{:?}",
            asked.lock().unwrap()
        );
        let helper: Value = serde_json::from_slice(
            &op.run(
                &repo,
                &env,
                &["config", "explain", "agents.configs.helper", "--json"],
                "",
            )
            .stdout,
        )
        .unwrap();
        assert_eq!(helper["value"]["model"], "m1", "{helper}");
        assert!(
            helper["value"]["account"].is_null(),
            "a repository chose who pays: {helper}"
        );

        std::fs::write(repo.join(".legionctl/broken.jsonc"), r#"{"load": 5}"#).unwrap();
        let held = prompt();
        assert!(
            held.contains(r#""decision":"block""#)
                && held.contains("broken.jsonc")
                && held.contains("load"),
            "a broken file must hold the prompt and name itself: {held}"
        );
    }

    /// A-6: a repository team of the plugin's name is not the one used; its own name waits for the user's answer.
    fn repository_teams_wait(
        op: &Operator,
        (home, repo): (&std::path::Path, &std::path::Path),
        asked: &std::sync::Mutex<Vec<String>>,
    ) {
        let env = [("HOME", home.to_str().unwrap())];
        // a repository team of the plugin's name is not the one used; its own name waits for the user
        std::fs::create_dir_all(repo.join(".legionctl/teams")).unwrap();
        std::fs::write(repo.join(".legionctl/teams/vanilla.jsonc"), "not a team").unwrap();
        let vanilla = op.run(
            repo,
            &env,
            &["runnables", "check", "--team", "vanilla", "--json"],
            "",
        );
        assert!(vanilla.status.success(), "the plugin's team: {vanilla:?}");
        std::fs::write(
            repo.join(".legionctl/teams/rogue.jsonc"),
            r#"{"name": "rogue", "members": {"main0": {"agent": "main-agent"}}}"#,
        )
        .unwrap();
        let rogue = op.run(
            repo,
            &env,
            &["runnables", "check", "--team", "rogue", "--json"],
            "",
        );
        assert!(
            !rogue.status.success()
                && String::from_utf8_lossy(&rogue.stderr).contains("not used until you allow it"),
            "{rogue:?}"
        );
        answered(home, "rogue.jsonc", asked, 2);
        assert!(
            asked.lock().unwrap()[1].contains("team rogue"),
            "{:?}",
            asked.lock().unwrap()
        );
        let allowed = op.run(
            repo,
            &env,
            &["runnables", "check", "--team", "rogue", "--json"],
            "",
        );
        assert!(allowed.status.success(), "{allowed:?}");
    }

    /// A-4, A-6 (EV-review-14): a repository's view (its buttons run operations) and its team (agents, models,
    /// instructions) are used only after the user allows their content, and neither hides the user's or the plugin's
    /// of the same name: the user's view shows over the repository's, the plugin's team over the repository's.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_repositorys_views_and_teams_wait_for_the_user_and_hide_none_of_the_users() {
        let m = Machine::new("rviews");
        let (_stand_ins, _asked, (_bus, _daemon)) = start(&m).await;
        let address = format!("unix:path={}", m.dir.join("bus").display());
        let (_desktop, asked) = crate::desktop::answering(&address, &["allow", "allow"]).await;
        let (op, home, repo) = tofu_operator(&m, (m.tag, address));
        std::fs::remove_dir_all(repo.join(".legionctl")).unwrap();
        let view = |name: &str, title: &str| {
            serde_json::json!({"name": name, "title": title, "widget": "list", "source": {"op": "about"},
                               "columns": ["part"],
                               "actions": [{"label": "Go", "op": "hooks.add", "input": {"id": "x", "command": "true"}}]})
            .to_string()
        };
        std::fs::create_dir_all(repo.join(".legionctl/views")).unwrap();
        std::fs::create_dir_all(home.join(".config/legionctl/views")).unwrap();
        std::fs::write(
            repo.join(".legionctl/views/panel.jsonc"),
            view("panel", "Repo panel"),
        )
        .unwrap();
        std::fs::write(
            repo.join(".legionctl/views/shared.jsonc"),
            view("shared", "Repo shared"),
        )
        .unwrap();
        std::fs::write(
            home.join(".config/legionctl/views/shared.jsonc"),
            view("shared", "My shared"),
        )
        .unwrap();
        let env = [("HOME", home.to_str().unwrap())];
        let list = || -> Value {
            serde_json::from_slice(&op.run(&repo, &env, &["views", "list", "--json"], "").stdout)
                .unwrap()
        };
        let find = |all: &Value, name: &str| -> Value {
            all.as_array()
                .unwrap()
                .iter()
                .find(|v| v["name"] == name)
                .cloned()
                .unwrap_or(Value::Null)
        };
        let before = list();
        assert!(
            find(&before, "panel")["error"]
                .as_str()
                .is_some_and(|e| e.contains("not used until you allow it")),
            "{before:#}"
        );
        assert_eq!(find(&before, "shared")["title"], "My shared", "{before:#}");
        assert_eq!(find(&before, "shared")["layer"], "user", "{before:#}");
        answered(&home, "panel.jsonc", &asked, 1);
        let after = list();
        assert_eq!(find(&after, "panel")["title"], "Repo panel", "{after:#}");
        assert_eq!(
            find(&after, "shared")["title"],
            "My shared",
            "the user's view stays over the repository's"
        );

        // a repository's buttons confirm every change; the user's own run their class route
        let confirm = |name: &str| -> Value {
            let shown: Value = serde_json::from_slice(
                &op.run(&repo, &env, &["views", "render", name, "--json"], "")
                    .stdout,
            )
            .unwrap();
            shown["actions"][0]["confirm"].clone()
        };
        assert_eq!(
            (confirm("panel"), confirm("shared")),
            (Value::Bool(true), Value::Bool(false))
        );
        // the user's own save into the repository's layer is the user's answer for that content: no question
        let saved = op.run(
            &repo,
            &env,
            &["views", "save", "--project"],
            &view("panel", "Repo panel, mine"),
        );
        assert!(saved.status.success(), "{saved:?}");
        let panel = find(&list(), "panel");
        assert_eq!(panel["title"], "Repo panel, mine");
        assert_eq!(panel["layer"], "repository", "the project's layer: {panel}");
        assert_eq!(
            asked.lock().unwrap().len(),
            1,
            "{:?}",
            asked.lock().unwrap()
        );

        repository_teams_wait(&op, (&home, &repo), &asked);
    }

    /// The operator of the compose test on the private bus (also its desktop) with this machine's Docker, and its
    /// repository with one commit.
    fn compose_operator(
        m: &Machine,
        address: String,
    ) -> (Operator, std::path::PathBuf, std::path::PathBuf) {
        let home = m.dir.join("home");
        let repo = home.join("trustcompose");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            &["init", "-q"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "start",
            ],
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&repo)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let path = std::env::join_paths(
            std::iter::once(m.dir.join("bin"))
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let op = Operator::start(
            "compose",
            &[
                ("PATH", path),
                ("HOME", home.clone().into()),
                ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
                ("DBUS_SYSTEM_BUS_ADDRESS", address.clone().into()),
                ("DBUS_SESSION_BUS_ADDRESS", address.into()),
                (
                    "DOCKER_HOST",
                    std::env::var_os("DOCKER_HOST")
                        .unwrap_or_else(|| "unix:///var/run/docker.sock".into()),
                ),
            ],
        );
        (op, home, repo)
    }

    /// A-5 (EV-review-14): `images run` starts a repository's own compose file on the host only after the user
    /// allowed its content (as docker compose reads it), and the question names each service that gets root on the
    /// machine; after Deny nothing starts, a changed file asks again, and after Allow compose runs (here it stops at
    /// the image that is not there, so no container starts). This machine's Docker reads the files; nothing runs.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_repositorys_compose_file_runs_only_after_the_user_allows_it_and_names_its_root_services()
     {
        let m = Machine::new("compose");
        let (_stand_ins, _asked, (_bus, _daemon)) = start(&m).await;
        let address = format!("unix:path={}", m.dir.join("bus").display());
        let (_desktop, asked) = crate::desktop::answering(&address, &["deny", "allow"]).await;
        let (op, home, repo) = compose_operator(&m, address);
        let compose = |privileged: bool| {
            std::fs::write(
                repo.join("compose.yaml"),
                format!(
                    "services:\n  app:\n    image: localhost/trustcompose-missing:none\n    pull_policy: never\n\
                     \x20   privileged: {privileged}\n"
                ),
            )
            .unwrap();
        };
        let env = [("HOME", home.to_str().unwrap())];
        let run = || op.run(&repo, &env, &["images", "run"], "");
        compose(true);
        let first = run();
        assert!(
            !first.status.success()
                && String::from_utf8_lossy(&first.stderr).contains("not used until you allow it"),
            "{first:?}"
        );
        answered(&home, "compose.yaml", &asked, 1);
        assert!(
            asked.lock().unwrap()[0].contains("service app with root on this machine"),
            "{:?}",
            asked.lock().unwrap()
        );
        let denied = run();
        assert!(
            String::from_utf8_lossy(&denied.stderr).contains("you denied"),
            "{denied:?}"
        );
        compose(false);
        let _ = run();
        answered(&home, "compose.yaml", &asked, 2);
        let approvals = home.join(".local/state/legionctl/approvals.json");
        for _ in 0..200 {
            if std::fs::read_to_string(&approvals)
                .unwrap_or_default()
                .contains("true")
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            !asked.lock().unwrap()[1].contains("with root"),
            "{:?}",
            asked.lock().unwrap()
        );
        let allowed = run();
        let _ = std::process::Command::new("docker")
            .current_dir(&repo)
            .args([
                "compose",
                "-p",
                "trustcompose",
                "-f",
                "compose.yaml",
                "-f",
                ".legionctl/compose.json",
                "down",
            ])
            .output();
        assert!(
            String::from_utf8_lossy(&allowed.stderr).contains("docker compose up"),
            "past the gate, compose itself refuses the missing image: {allowed:?}"
        );
    }

    /// B-1 (EV-review-14): a sandbox reads only its own events (what it ran and its own calls): the log holds the
    /// command lines of every session; the user reads all of them.
    #[test]
    fn a_sandbox_reads_only_its_own_events() {
        let home = Scratch::new(format!("/var/tmp/legionctl-evs-{}", std::process::id()));
        let op = Operator::start("evs", &[("HOME", home.as_os_str().to_owned())]);
        op.ok(&["events"]);
        add_session_token(&home, "agent-token", "agent-1", &[]);
        let added = std::process::Command::new("python3")
            .args([
                "-c",
                "import json, sqlite3, sys; c = sqlite3.connect(sys.argv[1]); \
                 [c.execute('INSERT INTO events (ts, actor, kind, payload) VALUES (0, ?, ?, ?)', r) for r in [\
                 ('daemon', 'session.exec', json.dumps({'session': 'agent-1', 'pid': 1, 'argv': ['OWN_RUN']})), \
                 ('daemon', 'session.exec', json.dumps({'session': 'other', 'pid': 2, 'argv': ['OTHER_SECRET']})), \
                 ('cli', 'hooks.add', json.dumps({'principal': {'session': 'agent-1'}, 'input': 'OWN_CALL'}))]]; c.commit()",
            ])
            .arg(home.join(".local/state/legionctl/operator.db"))
            .status()
            .unwrap();
        assert!(added.success());
        let agent = op.run(
            &home,
            &[
                ("LEGIONCTL_TOKEN", "agent-token"),
                ("HOME", home.to_str().unwrap()),
            ],
            &["events", "--json"],
            "",
        );
        let seen = String::from_utf8_lossy(&agent.stdout);
        assert!(
            seen.contains("OWN_RUN") && seen.contains("OWN_CALL") && !seen.contains("OTHER_SECRET"),
            "{seen} {}",
            String::from_utf8_lossy(&agent.stderr)
        );
        let all = op.ok(&["events", "--json"]);
        assert!(
            all.contains("OTHER_SECRET") && all.contains("OWN_RUN"),
            "{all}"
        );
        drop(op);
    }

    /// U-12: a question that waits for the user can be answered outside its desktop notice (the app's Activity, a
    /// terminal): `approval answer <ask> allow` lets the agent's call go ahead and closes the notice; an agent cannot
    /// answer, and a question that has its answer does not wait any more.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_user_answers_an_agents_question_outside_its_notice_and_the_notice_closes() {
        let m = Machine::new("answer");
        let (_stand_ins, _asked, (_bus, _daemon)) = start(&m).await;
        let address = format!("unix:path={}", m.dir.join("bus").display());
        let (_desktop, asked) = crate::desktop::answering(&address, &["wait"]).await;
        let home = m.dir.join("home");
        let op = std::sync::Arc::new(Operator::start(
            "answer",
            &[
                ("PATH", m.dir.join("bin").into()),
                ("HOME", home.clone().into()),
                ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
                ("DBUS_SYSTEM_BUS_ADDRESS", address.clone().into()),
                ("DBUS_SESSION_BUS_ADDRESS", address.into()),
            ],
        ));
        op.ok(&["events"]);
        add_session_token(&home, "agent-token", "agent-1", &[]);
        let agent = {
            let (op, home) = (op.clone(), home.clone());
            move |args: &[&str]| {
                op.run(
                    &home,
                    &[
                        ("LEGIONCTL_TOKEN", "agent-token"),
                        ("HOME", home.to_str().unwrap()),
                    ],
                    args,
                    "",
                )
            }
        };
        let waiting = {
            let agent = agent.clone();
            std::thread::spawn(move || agent(&["hooks", "add", "greet", "echo hello"]))
        };
        let ask = loop {
            let events: Value = serde_json::from_str(&op.ok(&["events", "--json"])).unwrap();
            if let Some(e) = events["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["kind"] == "approval.ask")
            {
                break e["payload"]["ask"].as_u64().unwrap().to_string();
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        let refused = agent(&["approval", "answer", &ask, "allow"]);
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("only the user answers a question"),
            "{refused:?}"
        );
        assert_eq!(
            op.ok(&["approval", "answer", &ask, "allow"]).trim(),
            format!("question {ask}: allowed")
        );
        let done = waiting.join().unwrap();
        assert!(
            done.status.success(),
            "{}",
            String::from_utf8_lossy(&done.stderr)
        );
        assert_eq!(asked.lock().unwrap().last().unwrap(), "closed 1");
        let again = op.cli(&["approval", "answer", &ask, "deny"]);
        assert!(
            String::from_utf8_lossy(&again.stderr).contains("does not wait now"),
            "{again:?}"
        );
    }

    /// The pre-tool hook of agent session `plain-1` (Claude Code's Write) denies a write into the user's legionctl
    /// answers and settings, and lets a write elsewhere through.
    fn agent_writes_into_the_settings_are_denied(
        home: &std::path::Path,
        agent: &impl Fn(&[&str], &str) -> Output,
    ) {
        for file in [
            home.join(".local/state/legionctl/approvals.json"),
            home.join(".config/legionctl/managed.jsonc"),
        ] {
            let write = agent(
            &["hook", "pre-tool"],
            &serde_json::json!({"session_id": "plain-1", "cwd": home, "hook_event_name": "PreToolUse",
                                "tool_name": "Write", "tool_input": {"file_path": file, "content": "{}"}})
            .to_string(),
        );
            let answer = String::from_utf8_lossy(&write.stdout);
            assert!(
                answer.contains(r#""permissionDecision":"deny""#)
                    && answer.contains("change them with legionctl"),
                "{}: {answer} {}",
                file.display(),
                String::from_utf8_lossy(&write.stderr)
            );
        }
        let elsewhere = agent(
        &["hook", "pre-tool"],
        &serde_json::json!({"session_id": "plain-1", "cwd": home, "hook_event_name": "PreToolUse",
                            "tool_name": "Write", "tool_input": {"file_path": home.join("notes.md"), "content": "x"}})
        .to_string(),
    );
        assert!(
            !String::from_utf8_lossy(&elsewhere.stdout).contains("deny"),
            "{elsewhere:?}"
        );
    }

    /// A-3 (EV-review-14): an agent that is not sandboxed (Claude Code in the user's terminal) holds the user's token
    /// but is not the user. Its change of the reminders that every session gets waits for the user (Deny refuses it,
    /// Allow lets it go on); it cannot answer a question; the pre-tool hook denies its file writes into the user's
    /// legionctl settings and answers. The user's own terminal asks nothing and answers the question.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unsandboxed_agent_is_not_the_user_for_questions_reminders_and_hooks() {
        let m = Machine::new("plain");
        let (_stand_ins, _asked, (_bus, _daemon)) = start(&m).await;
        let address = format!("unix:path={}", m.dir.join("bus").display());
        let (_desktop, asked) =
            crate::desktop::answering(&address, &["deny", "allow", "wait"]).await;
        let home = m.dir.join("home");
        let op = std::sync::Arc::new(Operator::start(
            "plain",
            &[
                ("PATH", m.dir.join("bin").into()),
                ("HOME", home.clone().into()),
                ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
                ("DBUS_SYSTEM_BUS_ADDRESS", address.clone().into()),
                ("DBUS_SESSION_BUS_ADDRESS", address.into()),
            ],
        ));
        op.ok(&["events"]);
        let agent = {
            let (op, home) = (op.clone(), home.clone());
            move |args: &[&str], stdin: &str| {
                op.run(
                    &home,
                    &[
                        ("CLAUDE_CODE_SESSION_ID", "plain-1"),
                        ("HOME", home.to_str().unwrap()),
                    ],
                    args,
                    stdin,
                )
            }
        };
        let user = |args: &[&str]| op.run(&home, &[("HOME", home.to_str().unwrap())], args, "");
        let add = ["reminders", "add", "greet", "Say hello first"];
        let denied = agent(&add, "");
        assert!(
            !denied.status.success()
                && String::from_utf8_lossy(&denied.stderr).contains("the user did not allow it"),
            "{denied:?}"
        );
        let allowed = agent(&add, "");
        assert!(allowed.status.success(), "{allowed:?}");
        assert_eq!(
            asked.lock().unwrap().len(),
            2,
            "{:?}",
            asked.lock().unwrap()
        );
        assert!(
            asked.lock().unwrap()[0].contains("plain-1"),
            "{:?}",
            asked.lock().unwrap()
        );
        let off = user(&["reminders", "off", "greet"]);
        assert!(off.status.success(), "{off:?}");
        assert_eq!(
            asked.lock().unwrap().len(),
            2,
            "the user's own call asks nothing"
        );

        let waiting = {
            let agent = agent.clone();
            std::thread::spawn(move || agent(&["hooks", "add", "greet", "echo hello"], ""))
        };
        let ask = loop {
            let events: Value = serde_json::from_str(&op.ok(&["events", "--json"])).unwrap();
            if let Some(e) = events["items"].as_array().unwrap().iter().find(|e| {
                e["kind"] == "approval.ask"
                    && e["payload"]["what"].to_string().contains("echo hello")
            }) {
                break e["payload"]["ask"].as_u64().unwrap().to_string();
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        let refused = agent(&["approval", "answer", &ask, "allow"], "");
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("only the user answers a question"),
            "{refused:?}"
        );
        assert!(
            user(&["approval", "answer", &ask, "allow"])
                .status
                .success()
        );
        assert!(waiting.join().unwrap().status.success());

        agent_writes_into_the_settings_are_denied(&home, &agent);
    }

    /// C-8 (EV-review-14): an operation that changes the user's files without the daemon (here the user's own rule
    /// layer, which the root daemon then applies) is no machine change that polkit guards: a sandboxed agent's call
    /// waits for the user, and after Deny no layer is written. The user's own call applies at once (the configsets
    /// test above).
    #[tokio::test(flavor = "multi_thread")]
    async fn an_agent_asks_the_user_before_it_writes_the_users_rule_layer() {
        let m = Machine::new("ownlayer");
        let (_stand_ins, _asked, (_bus, _daemon)) = start(&m).await;
        let address = format!("unix:path={}", m.dir.join("bus").display());
        let (_desktop, asked) = crate::desktop::answering(&address, &["deny"]).await;
        let home = m.dir.join("home");
        let op = Operator::start(
            "ownlayer",
            &[
                ("PATH", m.dir.join("bin").into()),
                ("HOME", home.clone().into()),
                ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
                ("DBUS_SYSTEM_BUS_ADDRESS", address.clone().into()),
                ("DBUS_SESSION_BUS_ADDRESS", address.into()),
            ],
        );
        op.ok(&["events"]);
        add_session_token(&home, "agent-token", "agent-1", &[]);
        let denied = op.run(
            &home,
            &[
                ("LEGIONCTL_TOKEN", "agent-token"),
                ("HOME", home.to_str().unwrap()),
            ],
            &["rules", "edit", "50-mine", "sysctl.vm.swappiness=60"],
            "",
        );
        assert!(
            !denied.status.success()
                && String::from_utf8_lossy(&denied.stderr).contains(
                    "rules.edit changes more than this session, and the user did not allow it"
                ),
            "{denied:?}"
        );
        assert_eq!(asked.lock().unwrap().len(), 1);
        assert!(
            !home
                .join(".config/legionctl/configsets/50-mine.jsonc")
                .exists()
        );
    }

    /// An agent's call outside its own session waits for the user (DESIGN/03): a `workspace` op from a session's
    /// token shows a desktop notification with Allow and Deny (a stand-in desktop here, which denies the first and
    /// allows the second); Deny is the `denied` code; the session's own ops ask nothing; the user's own calls ask
    /// nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_agent_asks_the_user_before_it_changes_more_than_its_session() {
        let m = Machine::new("approve");
        let (_stand_ins, _asked, (_bus, _daemon)) = start(&m).await;
        let address = format!("unix:path={}", m.dir.join("bus").display());
        let (desktop, asked) = crate::desktop::answering(&address, &["deny", "allow"]).await;
        let home = m.dir.join("home");
        let op = Operator::start(
            "approve",
            &[
                ("PATH", m.dir.join("bin").into()),
                ("HOME", home.clone().into()),
                ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
                ("DBUS_SYSTEM_BUS_ADDRESS", address.clone().into()),
                ("DBUS_SESSION_BUS_ADDRESS", address.into()),
            ],
        );
        // `events` opens the operator database, so its tables exist
        op.ok(&["events"]);
        add_session_token(&home, "agent-token", "agent-1", &[]);
        let agent = |args: &[&str]| {
            op.run(
                &home,
                &[
                    ("LEGIONCTL_TOKEN", "agent-token"),
                    ("HOME", home.to_str().unwrap()),
                ],
                args,
                "",
            )
        };
        let hooks_add = ["hooks", "add", "greet", "echo hello"];

        let denied = agent(&hooks_add);
        assert!(!denied.status.success());
        assert!(
            String::from_utf8_lossy(&denied.stderr).contains(
                "hooks.add changes more than this session, and the user did not allow it"
            ),
            "{}",
            String::from_utf8_lossy(&denied.stderr)
        );
        let allowed = agent(&hooks_add);
        assert!(
            allowed.status.success(),
            "{}",
            String::from_utf8_lossy(&allowed.stderr)
        );
        let own = agent(&["dossier", "use", "box"]);
        assert!(
            own.status.success(),
            "{}",
            String::from_utf8_lossy(&own.stderr)
        );
        op.ok(&["hooks", "rm", "greet"]);

        let asked = asked.lock().unwrap().clone();
        assert_eq!(
            asked.len(),
            2,
            "only the agent's two workspace calls ask: {asked:?}"
        );
        // the user reads what the agent wants in words, not an operation name and its JSON (U-12)
        assert_eq!(
            asked[0],
            format!(
                "Define your own action: a one-line shell command whose output goes to the agent (id greet, command \
                 echo hello).\nAgent session agent-1 asks for it in {}.",
                home.display()
            ),
        );
        let audit: Value = serde_json::from_str(&op.ok(&["events", "--json"])).unwrap();
        let outcomes: Vec<(String, Value, Value)> = audit["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "hooks.add")
            .map(|e| {
                (
                    e["kind"].as_str().unwrap().to_string(),
                    e["payload"]["principal"].clone(),
                    e["payload"]["outcome"].clone(),
                )
            })
            .collect();
        assert_eq!(
            outcomes,
            [(
                "hooks.add".into(),
                serde_json::json!({"session": "agent-1"}),
                Value::Null
            )],
            "the denied call never ran; the allowed one is the agent's"
        );
        drop(desktop);
    }

    /// DESIGN/22 rule 2: a harness agent's scope is the read class and the toolsets of the skills it loaded. A call
    /// outside them asks the user for the whole toolset that holds it (Deny refuses it, Allow keeps the toolset in the
    /// agent's grant, so the next call asks nothing); loading a skill (Claude Code's recorded Skill call) is the
    /// declaration of its toolsets and asks nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_harness_agent_holds_its_skills_toolsets_and_the_user_allows_one_more() {
        let m = Machine::new("scope");
        let (_stand_ins, _asked, (_bus, _daemon)) = start(&m).await;
        let address = format!("unix:path={}", m.dir.join("bus").display());
        let (desktop, asked) = crate::desktop::answering(&address, &["deny", "allow"]).await;
        let home = m.dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let op = Operator::start(
            "scope",
            &[
                ("PATH", m.dir.join("bin").into()),
                ("HOME", home.clone().into()),
                ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
                ("DBUS_SYSTEM_BUS_ADDRESS", address.clone().into()),
                ("DBUS_SESSION_BUS_ADDRESS", address.into()),
            ],
        );
        let env = [
            ("LEGIONCTL_SESSION", "h1"),
            ("HOME", home.to_str().unwrap()),
        ];
        let agent = |args: &[&str]| op.run_bare(&home, &env, args, "");
        let err = |o: &std::process::Output| String::from_utf8_lossy(&o.stderr).to_string();

        assert!(
            agent(&["dossier", "list"]).status.success(),
            "a read asks nothing"
        );
        let denied = agent(&["dossier", "use", "box"]);
        assert!(
            err(&denied).contains(
                "dossier.use needs the toolset dossier-write, and the user did not allow it"
            ),
            "{}",
            err(&denied)
        );
        let mut skill: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/claude-code-hooks/post-tool-skill.json"
        ))
        .unwrap();
        skill["session_id"] = "h1".into();
        skill["cwd"] = home.to_str().unwrap().into();
        let loaded = op.run_bare(&home, &env, &["hook", "post-tool"], &skill.to_string());
        assert!(loaded.status.success(), "{}", err(&loaded));
        let used = agent(&["dossier", "use", "box"]);
        assert!(
            used.status.success(),
            "the loaded skill's toolset: {}",
            err(&used)
        );

        let fact = |value: &str| agent(&["facts", "set", "app", value, "--attribute", "width"]);
        let first = fact("0.3");
        assert!(first.status.success(), "{}", err(&first));
        let again = fact("0.4");
        assert!(again.status.success(), "{}", err(&again));
        let asked = asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 2, "one question per toolset: {asked:?}");
        assert!(
            asked[0].starts_with("Agent session h1 asks for the toolset dossier-write:")
                && asked[1].starts_with("Agent session h1 asks for the toolset session-own:")
                && asked[1].contains("Its first use: "),
            "{asked:?}"
        );
        let scope: Value = serde_json::from_slice(
            &agent(&[
                "runnables",
                "scope",
                "--json",
                "dossier.use",
                "facts.set",
                "hooks.add",
            ])
            .stdout,
        )
        .unwrap();
        assert_eq!(
            scope,
            serde_json::json!({"sets": ["dossier-write", "session-own"],
                               "missing": {"hooks.add": "settings-change"}}),
            "the grant keeps both toolsets"
        );
        drop(desktop);
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
            "Windows boots from firmware entry 0004 (Windows Boot Manager)\n\
             registry: not checked (mount the Windows volume read-only with ntfs3 to check it)\n"
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
                .contains("not rebooting: run: efibootmgr --bootnext 0004 failed: efibootmgr --bootnext 0004 exited with exit status: 1: Could not set BootNext"),
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

/// Resources after the start (todo-85, note 22): one list, and one action that runs an existing operation.
mod resources {
    use super::daemon_process::{daemon, start, wait_for};
    use super::operator_and_daemon::{boot_machine, on_bus, qmp_stand_in, windows_machine};
    use super::*;
    use serde_json::Value;

    const SWAPPINESS: &str = "/proc/sys/vm/swappiness";

    fn json(op: &Operator, args: &[&str]) -> Value {
        serde_json::from_str(&op.ok(&[args, &["--json"]].concat())).unwrap()
    }

    /// The resource `id` of `resources list`.
    fn resource(op: &Operator, id: &str) -> Value {
        let list = json(op, &["resources", "list"]);
        list["resources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("no {id} in {list}"))
    }

    /// (name, operation, class) of each action of a resource.
    fn actions(r: &Value) -> Vec<(String, String, String)> {
        r["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                (
                    a["name"].as_str().unwrap().into(),
                    a["op"].as_str().unwrap().into(),
                    a["class"].as_str().unwrap().into(),
                )
            })
            .collect()
    }

    fn triples(list: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
        list.iter()
            .map(|(a, b, c)| ((*a).into(), (*b).into(), (*c).into()))
            .collect()
    }

    /// The kinds of the events after event `after`, once the recorder has them (the daemon's signals come late).
    pub(super) fn kinds_after(op: &Operator, after: i64) -> Vec<String> {
        json(op, &["events", "--limit", "1000"])["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["id"].as_i64().unwrap() > after)
            .map(|e| e["kind"].as_str().unwrap().to_string())
            .collect()
    }

    pub(super) fn last_event(op: &Operator) -> i64 {
        json(op, &["events", "--limit", "1000"])["items"]
            .as_array()
            .unwrap()
            .last()
            .map_or(0, |e| e["id"].as_i64().unwrap())
    }

    /// A session of the stand-in agent `probe` in `repo`; its id.
    fn probe_session(op: &Operator, repo: &Path) -> String {
        let started = op.run(
            repo,
            &[],
            &["session", "start", "probe", "--dossier", "res", "--json"],
            "",
        );
        assert!(
            started.status.success(),
            "{}",
            String::from_utf8_lossy(&started.stderr)
        );
        serde_json::from_slice::<Value>(&started.stdout).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// polkit was asked for `action`.
    fn was_asked(asked: &super::daemon_process::Asked, action: &str) -> bool {
        asked.lock().unwrap().iter().any(|(_, a, _, _)| a == action)
    }

    /// The recorded machine with its Windows disk (`windows_machine`): the data partition mounted with `vim` working in
    /// it, `steam` with the NVIDIA device open, a `systemctl` that logs each call and runs and freezes session
    /// scopes, and a repository for a session. Returns the stand-in kernel's thread and the repository.
    fn resource_machine(m: &Machine) -> (std::thread::JoinHandle<()>, PathBuf) {
        let kernel = windows_machine(m);
        m.set(
            "/proc/1/mounts",
            "/dev/nvme1n1p2 / btrfs rw 0 0\n/dev/nvme0n1p4 /mnt/windows-data ntfs3 rw 0 0",
        );
        let link = |from: &str, to: &str| {
            let at = m.dir.join("root").join(from);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(to, at).unwrap();
        };
        link("proc/4242/cwd", "/mnt/windows-data/docs");
        m.set("/proc/4242/comm", "vim");
        link("proc/4343/cwd", "/home/u");
        link("proc/4343/fd/7", "/dev/nvidia0");
        m.set("/proc/4343/comm", "steam");
        // the systemctl of windows_machine, and a session scope that runs and freezes
        let log = m.dir.join("systemctl.log");
        let freezer = m.dir.join("freezer");
        std::fs::write(&freezer, "running").unwrap();
        Machine::script(
            &m.dir.join("bin"),
            "systemctl",
            &format!(
                "echo \"$*\" >> {log}; case \"$*\" in *is-active*legionctl-session*) echo active;; \
                 *is-active*) exit 3;; *FreezerState*) /usr/bin/cat {f};; *freeze*) echo frozen > {f};; esac",
                log = log.display(),
                f = freezer.display()
            ),
        );
        let home = m.dir.join("home");
        let repo = home.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            home.join(".gitconfig"),
            "[user]\n  name = Test\n  email = test@example.com\n",
        )
        .unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "x",
            ],
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&repo)
                    .env("HOME", &home)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        std::fs::write(
            home.join(".config/legionctl/session.jsonc"),
            serde_json::json!({
                "session": {"agents": {"probe": ["true"]}, "writable": []},
                "workspace": {"root": home.join("worktrees")}
            })
            .to_string(),
        )
        .unwrap();
        // what a session start runs; the rest of the operator's PATH stays stand-ins only
        for tool in ["git", "true"] {
            std::os::unix::fs::symlink(format!("/usr/bin/{tool}"), m.dir.join("bin").join(tool))
                .unwrap();
        }
        (kernel, repo)
    }

    /// The groups, states, users and actions that `resources list` shows on the resource machine.
    fn assert_listed(op: &Operator, session: &str) {
        let data = resource(op, "mount:/mnt/windows-data");
        assert_eq!(
            (&data["group"], &data["state"], &data["users"]),
            (
                &"disk".into(),
                &"mounted".into(),
                &serde_json::json!(["4242 vim"])
            ),
            "{data}"
        );
        assert_eq!(
            actions(&data),
            triples(&[("detach", "windows.disk", "machine")])
        );
        let system = resource(op, "mount:/mnt/windows-system");
        assert_eq!(system["state"], "not mounted");
        assert_eq!(
            actions(&system),
            triples(&[
                ("attach", "windows.disk", "machine"),
                ("detach", "windows.disk", "machine")
            ])
        );
        let root_fs = resource(op, "mount:/");
        assert_eq!(
            (actions(&root_fs).len(), &root_fs["users"]),
            (0, &serde_json::json!([])),
            "the root file system has no action, and is not every process's: {root_fs}"
        );
        let nvidia = resource(op, "gpu:nvidia");
        assert_eq!(
            (&nvidia["state"], &nvidia["users"]),
            (&"linux".into(), &serde_json::json!(["4343 steam"])),
            "{nvidia}"
        );
        assert_eq!(
            actions(&nvidia),
            triples(&[
                ("give-windows", "gpu.give", "machine"),
                ("give-none", "gpu.give", "machine")
            ])
        );
        let knob = resource(op, "knob:sysctl.vm.swappiness");
        assert_eq!(knob["state"], "held at 10");
        assert_eq!(
            actions(&knob),
            triples(&[("release", "knobs.release", "machine")])
        );
        let running = resource(op, &format!("session:{session}"));
        assert_eq!(running["state"], "running");
        assert_eq!(
            actions(&running),
            triples(&[
                ("pause", "session.pause", "workspace"),
                ("stop", "session.stop", "workspace")
            ])
        );
        assert_eq!(resource(op, "layer:shipped/20-ac")["state"], "on, used now");
        let windows = resource(op, "vm:windows");
        assert_eq!(windows["state"], "stopped");
        assert_eq!(
            actions(&windows),
            triples(&[("start", "windows.vm.start", "machine")])
        );
    }

    /// A Windows disk that cannot be read (its by-uuid links are not readable) fails `resources list` with the path:
    /// it is not shown as "no Windows disk" (no VM, no attach or detach) as if the machine had none.
    #[tokio::test(flavor = "multi_thread")]
    async fn resources_list_fails_when_the_windows_disk_cannot_be_read() {
        use std::os::unix::fs::PermissionsExt;
        let m = Machine::new("resunread");
        let _kernel = windows_machine(&m);
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "resunread");
        assert_eq!(resource(&op, "vm:windows")["state"], "stopped");
        let links = m.dir.join("root/dev/disk/by-uuid");
        std::fs::set_permissions(&links, std::fs::Permissions::from_mode(0o000)).unwrap();
        let out = op.cli(&["resources", "list"]);
        std::fs::set_permissions(&links, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success() && err.contains("by-uuid"), "{err}");
    }

    /// The list shows each group with its state and users: the mounted Windows partition with the process in it, the
    /// NVIDIA GPU with the process that has it open, a held knob, a running session and the layers. Each act runs the
    /// existing operation as a call of its own (its effect, its polkit question, its event); the dry run of the disk
    /// detach shows the plan and changes nothing; an action that does not apply now is refused with nothing changed.
    #[tokio::test(flavor = "multi_thread")]
    async fn resources_show_each_group_and_each_action_runs_the_existing_operation() {
        let m = Machine::new("resources");
        let (_kernel, repo) = resource_machine(&m);
        let systemctl = || std::fs::read_to_string(m.dir.join("systemctl.log")).unwrap_or_default();
        let (_stand_ins, asked, _processes) = start(&m).await;
        let op = on_bus(&m, "resources");
        let session = probe_session(&op, &repo);
        op.ok(&["knobs", "hold", "sysctl.vm.swappiness", "10"]);
        assert_eq!(m.file(SWAPPINESS), "10");
        // the recorder subscribes after the operator's first call
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        assert_listed(&op, &session);

        // the dry run: the plan of the routed operation, nothing changed, no polkit question
        let before = last_event(&op);
        let dry = op.ok(&[
            "resources",
            "act",
            "mount:/mnt/windows-data",
            "detach",
            "--dry-run",
        ]);
        assert!(
            dry.contains("would run: systemctl stop mnt-windows\\x2dsystem.automount"),
            "{dry}"
        );
        assert!(!systemctl().contains("stop mnt-windows"), "{}", systemctl());
        assert!(!was_asked(&asked, "org.legionctl.windows.vm"));

        // each act: the routed operation's effect, polkit for a machine change, and its own event
        op.ok(&["resources", "act", "mount:/mnt/windows-data", "detach"]);
        assert!(
            systemctl().contains(
                "stop mnt-windows\\x2dsystem.automount mnt-windows\\x2dsystem.mount \
                 mnt-windows\\x2ddata.automount mnt-windows\\x2ddata.mount"
            ),
            "{}",
            systemctl()
        );
        op.ok(&["resources", "act", "knob:sysctl.vm.swappiness", "release"]);
        assert_eq!(
            json(&op, &["knobs", "show", "sysctl.vm.swappiness"])["knob"]["held"],
            Value::Null
        );
        assert_eq!(m.file(SWAPPINESS), "150", "the policy has the knob again");
        for action in ["org.legionctl.windows.vm", "org.legionctl.knobs.hold"] {
            assert!(was_asked(&asked, action), "{action}");
        }
        op.ok(&["resources", "act", &format!("session:{session}"), "pause"]);
        assert!(
            systemctl().contains(&format!("freeze legionctl-session-{session}.scope")),
            "{}",
            systemctl()
        );
        let paused = resource(&op, &format!("session:{session}"));
        assert_eq!(paused["state"], "paused", "{paused}");
        assert_eq!(
            actions(&paused),
            triples(&[
                ("resume", "session.unpause", "workspace"),
                ("stop", "session.stop", "workspace")
            ])
        );
        wait_for("the daemon's events", || {
            kinds_after(&op, before)
                .iter()
                .any(|k| k == "machine.release")
        });
        let kinds = kinds_after(&op, before);
        for kind in [
            "windows.disk",
            "machine.windows-disk",
            "knobs.release",
            "machine.release",
            "session.pause",
        ] {
            assert!(kinds.iter().any(|k| k == kind), "{kind}: {kinds:?}");
        }
        assert!(
            !kinds.iter().any(|k| k == "resources.act"),
            "the routed operation is the event: {kinds:?}"
        );

        // an action that does not apply now, and a resource that does not exist: refused, nothing changed
        // the reads (is-active, show) are not changes
        let changes = || {
            systemctl()
                .lines()
                .filter(|l| !l.contains("is-active") && !l.contains("show "))
                .count()
        };
        let changed_before = changes();
        let refused = op.cli(&["resources", "act", &format!("session:{session}"), "pause"]);
        assert!(
            String::from_utf8_lossy(&refused.stderr)
                .contains("has no action pause; its actions now: resume, stop"),
            "{refused:?}"
        );
        let unknown = op.cli(&["resources", "act", "vm:nowhere", "start"]);
        assert!(
            String::from_utf8_lossy(&unknown.stderr).contains("no resource vm:nowhere"),
            "{unknown:?}"
        );
        assert_eq!(changes(), changed_before, "{}", systemctl());
    }

    /// The state of the layer `id` in `resources list` (on, off: why).
    fn layer(op: &Operator, id: &str) -> String {
        resource(op, &format!("layer:{id}"))["state"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn held(op: &Operator, knob: &str) -> Value {
        json(op, &["knobs", "show", knob])["knob"]["held"].clone()
    }

    /// A saved set (note 22) keeps the layer states and the holds; a switch makes them the set's as one change (the
    /// holds it does not have are released, its own are written, the layer states follow), one event; its dry run
    /// changes nothing; undo of the switch event points to `switch previous`, which goes back, and a second time
    /// forward again.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_saved_set_switches_layers_and_holds_as_one_change_and_switch_previous_undoes_it() {
        let m = Machine::new("sets");
        let (_stand_ins, asked, _processes) = start(&m).await;
        let op = on_bus(&m, "sets");
        let max_perf = json(&op, &["knobs", "show", "cpu.max_perf_pct"])["knob"]["path"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(m.file(&max_perf), "100");
        op.ok(&["knobs", "hold", "sysctl.vm.swappiness", "10"]);
        op.ok(&["rules", "disable", "30-fnq-quiet"]);
        let saved = op.ok(&["rules", "sets", "save", "quiet"]);
        assert!(
            saved.contains("set quiet: 1 layer states, 1 holds"),
            "{saved}"
        );
        assert!(
            asked
                .lock()
                .unwrap()
                .iter()
                .any(|(_, a, _, _)| a == "org.legionctl.rules")
        );

        // another selection: no swappiness hold, the layer on, max_perf_pct held
        op.ok(&["knobs", "release", "sysctl.vm.swappiness"]);
        op.ok(&["rules", "enable", "30-fnq-quiet"]);
        op.ok(&["knobs", "hold", "cpu.max_perf_pct", "80"]);
        let other = || {
            assert_eq!(m.file(SWAPPINESS), "150");
            assert_eq!(m.file(&max_perf), "80");
            assert_eq!(held(&op, "cpu.max_perf_pct"), "80");
            assert_eq!(held(&op, "sysctl.vm.swappiness"), Value::Null);
            assert!(layer(&op, "shipped/30-fnq-quiet").starts_with("on"));
        };
        let quiet = || {
            assert_eq!(m.file(SWAPPINESS), "10");
            assert_eq!(m.file(&max_perf), "100", "released: the policy's value");
            assert_eq!(held(&op, "sysctl.vm.swappiness"), "10");
            assert_eq!(held(&op, "cpu.max_perf_pct"), Value::Null);
            assert!(layer(&op, "shipped/30-fnq-quiet").starts_with("off"));
        };
        other();

        let dry = op.ok(&["rules", "sets", "switch", "quiet", "--dry-run"]);
        assert_eq!(
            dry.lines().collect::<Vec<_>>(),
            [
                "layer shipped/30-fnq-quiet: on -> off",
                &format!("release {max_perf}"),
                "hold /proc/sys/vm/swappiness at 10",
            ],
            "{dry}"
        );
        other();

        let before = last_event(&op);
        op.ok(&["rules", "sets", "switch", "quiet"]);
        quiet();
        wait_for("the switch event", || {
            kinds_after(&op, before)
                .iter()
                .any(|k| k == "machine.switch-set")
        });
        let machine: Vec<String> = kinds_after(&op, before)
            .into_iter()
            .filter(|k| k.starts_with("machine."))
            .collect();
        assert_eq!(machine, ["machine.switch-set"], "one change");
        let event = json(&op, &["events", "--limit", "1000"])["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == "machine.switch-set")
            .unwrap()["id"]
            .to_string();
        let undo = op.cli(&["undo", &event]);
        assert!(
            String::from_utf8_lossy(&undo.stderr).contains("legionctl rules sets switch previous"),
            "{undo:?}"
        );
        quiet();

        op.ok(&["rules", "sets", "switch", "previous"]);
        other();
        op.ok(&["rules", "sets", "switch", "previous"]);
        quiet();

        let set = resource(&op, "set:quiet");
        assert_eq!(
            actions(&set),
            triples(&[
                ("switch", "rules.sets.switch", "machine"),
                ("remove", "rules.sets.remove", "machine")
            ])
        );
        assert_eq!(
            actions(&resource(&op, "set:previous")),
            triples(&[("switch", "rules.sets.switch", "machine")])
        );
        let reserved = op.cli(&["rules", "sets", "save", "previous"]);
        assert!(
            String::from_utf8_lossy(&reserved.stderr).contains("legionctl's own name"),
            "{reserved:?}"
        );
    }

    /// Boot entries (note 22): `--boot` is refused without the boot takeover, before polkit; with it, each kernel's
    /// image gets a profile for the set with `legionctl.set=<set>`, signed with the image, and the menu stays hidden; a
    /// set without `--boot` gets none. A daemon start with `legionctl.set=` applies the set for that start, and the next
    /// start without it puts the selection before back.
    #[tokio::test(flavor = "multi_thread")]
    async fn boot_entries_for_each_kernel_and_set_and_the_set_of_the_start_applies_at_daemon_start()
    {
        let m = Machine::new("bootsets");
        let _efivars = boot_machine(&m);
        let log = m.dir.join("ukify.log");
        Machine::script(
            &m.dir.join("bin"),
            "ukify",
            &format!(
                "printf '%s\\n' \"$*\" >> {}; out=; prev=; for a in \"$@\"; do [ \"$prev\" = --output ] && out=$a; \
                 prev=$a; done; [ -z \"$out\" ] || echo \"ukify $*\" > \"$out\"",
                log.display()
            ),
        );
        let ukify = || std::fs::read_to_string(&log).unwrap_or_default();
        let (stand_ins, asked, (_bus, first)) = start(&m).await;
        let op = on_bus(&m, "bootsets");
        let esp = m.dir.join("root/boot/efi");
        op.ok(&["knobs", "hold", "sysctl.vm.swappiness", "10"]);

        let refused = op.cli(&["rules", "sets", "save", "quiet", "--boot"]);
        assert!(
            String::from_utf8_lossy(&refused.stderr)
                .contains("install the boot takeover first (legionctl boot install)"),
            "{refused:?}"
        );
        assert!(m.ledger().sets().unwrap().is_empty(), "nothing saved");
        assert!(
            !asked
                .lock()
                .unwrap()
                .iter()
                .any(|(_, a, _, _)| a == "org.legionctl.rules"),
            "refused before the polkit dialog"
        );

        op.ok(&["boot", "install"]);
        assert!(!ukify().contains("--join-profile"), "no set, no profile");
        op.ok(&["rules", "sets", "save", "quiet", "--boot"]);
        op.ok(&["rules", "sets", "save", "plain"]);
        // the images made again (the pacman hook does it after a kernel update): still no entry for plain
        op.ok(&["boot", "rebuild"]);
        assert!(
            asked
                .lock()
                .unwrap()
                .iter()
                .any(|(_, a, _, _)| a == "org.legionctl.boot")
        );
        for kernel in ["linux-cachyos", "linux-cachyos-bore-lto"] {
            let image = esp.join(format!("EFI/Linux/legionctl-{kernel}.efi"));
            let profile = format!("{}.quiet.profile", image.display());
            assert!(
                ukify().contains(&format!(
                    "--profile ID=quiet\nTITLE={kernel} with quiet --cmdline quiet rw root=UUID=a9e6 \
                     legionctl.set=quiet --output {profile}"
                )),
                "{}",
                ukify()
            );
            let built = std::fs::read_to_string(&image).unwrap();
            assert!(
                built.contains(&format!(
                    "--profile TITLE={kernel} --join-profile {profile}"
                )) && built.contains("--secureboot-private-key")
                    && built.contains("--cmdline quiet rw root=UUID=a9e6 --"),
                "{built}"
            );
            assert!(
                !built.contains("plain"),
                "a set without --boot has no entry: {built}"
            );
            assert!(
                !Path::new(&profile).exists(),
                "the profile file goes after the build"
            );
        }
        let loader = std::fs::read_to_string(esp.join("loader/loader.conf")).unwrap();
        assert!(
            loader.contains("timeout 0\n"),
            "the menu stays hidden: {loader}"
        );

        // a start through the set's entry: the daemon applies the set for this start
        op.ok(&["knobs", "release", "sysctl.vm.swappiness"]);
        assert_eq!(m.file(SWAPPINESS), "150");
        drop(first);
        m.set(
            "/proc/cmdline",
            "quiet rw root=UUID=a9e6 legionctl.set=quiet initrd=\\initramfs-linux-cachyos.img",
        );
        let second = daemon(&m, &stand_ins).await;
        wait_for("the set at start", || m.file(SWAPPINESS) == "10");
        assert!(m.ledger().sets().unwrap().contains_key("before-boot"));
        // the next start without the parameter: the selection before
        drop(second);
        m.set(
            "/proc/cmdline",
            "quiet rw root=UUID=a9e6 initrd=\\initramfs-linux-cachyos.img",
        );
        let _third = daemon(&m, &stand_ins).await;
        // the daemon switches back first and drops before-boot after it: wait for both
        wait_for("the selection before at the next start", || {
            m.file(SWAPPINESS) == "150" && !m.ledger().sets().unwrap().contains_key("before-boot")
        });
        assert_eq!(held(&op, "sysctl.vm.swappiness"), Value::Null);
    }

    /// The Windows VM's pause and resume (note 22): without a running VM, a pause is refused and nothing reaches QMP,
    /// and the list offers start only; with it, pause sends QMP `stop`, the list says paused and offers resume, and
    /// resume sends `cont`.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_windows_vm_pauses_and_resumes_through_qmp_and_the_list_follows() {
        let m = Machine::new("vmpause");
        let _kernel = windows_machine(&m);
        let unit = m.dir.join("vm-unit");
        Machine::script(
            &m.dir.join("bin"),
            "systemctl",
            &format!(
                "case \"$*\" in *is-active*legionctl-windows-vm*) [ -e {} ];; *is-active*) exit 3;; esac",
                unit.display()
            ),
        );
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "vmpause");
        let qmp = || std::fs::read_to_string(m.dir.join("qmp.log")).unwrap_or_default();

        let refused = op.cli(&["windows", "vm", "pause"]);
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("no Windows VM runs"),
            "{refused:?}"
        );
        let stopped = resource(&op, "vm:windows");
        assert_eq!(stopped["state"], "stopped");
        assert_eq!(
            actions(&stopped),
            triples(&[("start", "windows.vm.start", "machine")])
        );
        let not_now = op.cli(&["resources", "act", "vm:windows", "pause"]);
        assert!(!not_now.status.success());
        assert_eq!(qmp(), "", "nothing reached QMP");

        std::fs::write(&unit, "").unwrap();
        let _qmp = qmp_stand_in(&m);
        let running = resource(&op, "vm:windows");
        assert_eq!(running["state"], "running");
        assert_eq!(
            actions(&running),
            triples(&[
                ("view", "windows.vm.view", "workspace"),
                ("pause", "windows.vm.pause", "workspace"),
                ("stop", "windows.vm.stop", "machine")
            ])
        );
        op.ok(&["resources", "act", "vm:windows", "pause"]);
        assert_eq!(qmp(), "stop {}\n");
        let paused = resource(&op, "vm:windows");
        assert_eq!(paused["state"], "paused");
        assert_eq!(
            actions(&paused),
            triples(&[
                ("resume", "windows.vm.resume", "workspace"),
                ("stop", "windows.vm.stop", "machine")
            ])
        );
        op.ok(&["resources", "act", "vm:windows", "resume"]);
        assert_eq!(qmp(), "stop {}\ncont {}\n");
        assert_eq!(resource(&op, "vm:windows")["state"], "running");
    }
}

/// Fan control through the CLI, the operator and the daemon, against the recorded `legionctl_wmi` hwmon device: what
/// reaches the module's files, in which order, and what never does.
mod fan_control {
    use super::daemon_process::{daemon, start, wait_for};
    use super::operator_and_daemon::on_bus;
    use super::resources::{kinds_after, last_event};
    use super::*;
    use serde_json::Value;

    /// stdout and stderr of a CLI call, and whether it succeeded.
    fn run(op: &Operator, args: &[&str]) -> (bool, String) {
        let out = op.cli(args);
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    async fn resume(stand_ins: &zbus::Connection) {
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
    }

    /// A resume with a governor drift, so that its repair is an event; returns when the event is there. `file` of the
    /// module is read-only meanwhile: a write to it fails the repair, and no event comes.
    async fn resume_without_a_write_to(
        m: &Machine,
        op: &Operator,
        stand_ins: &zbus::Connection,
        file: &str,
    ) {
        use std::os::unix::fs::PermissionsExt;
        let path = m.dir.join("root").join(&WMI_HWMON[1..]).join(file);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let before = last_event(op);
        m.set(
            "/sys/devices/system/cpu/cpufreq/policy3/scaling_governor",
            "powersave",
        );
        resume(stand_ins).await;
        wait_for("the resume repair", || {
            kinds_after(op, before)
                .iter()
                .any(|k| k == "machine.resume")
        });
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    /// Custom mode as the daemon knows it after the user entered it (the Fn+Q choice in the ledger).
    fn enter_custom(m: &Machine) {
        m.set(PROFILE_PATH, "custom");
        m.record(Key::Override, "custom");
    }

    const CURVE: [&str; 10] = ["1", "1", "2", "2", "3", "4", "5", "6", "8", "10"];

    fn set(op: &Operator, levels: &[&str]) -> (bool, String) {
        run(op, &[&["fan", "curve", "set"][..], levels].concat())
    }

    /// The point writes of a result line (`writes 10:10 9:8 ...`), each applied to `table` in order: every table
    /// between them must be one the module takes (each level 1-10, at least the point's minimum, not lower than the
    /// point before), else the module refuses that write on the real machine. Returns the last table.
    fn replay(text: &str, mut table: [u8; 10]) -> [u8; 10] {
        let writes = text
            .split("writes ")
            .nth(1)
            .and_then(|w| w.split(')').next())
            .unwrap_or_else(|| panic!("no writes in {text}"));
        for write in writes.split(' ') {
            let (point, level) = write.split_once(':').unwrap();
            table[point.parse::<usize>().unwrap() - 1] = level.parse().unwrap();
            let minimum = [1, 1, 1, 1, 1, 1, 1, 1, 3, 5];
            assert!(
                table
                    .iter()
                    .zip(minimum)
                    .all(|(l, min)| (min..=10).contains(l))
                    && table.windows(2).all(|w| w[0] <= w[1]),
                "the module refuses the table {table:?} after the write {write} of {writes}"
            );
        }
        table
    }

    /// Outside custom mode and for invalid levels a curve is refused with the reason, and nothing reaches the
    /// module; ends in custom mode.
    async fn refused_curves(m: &Machine, op: &Operator, stand_ins: &zbus::Connection) {
        let set = |levels: &[&str]| set(op, levels);
        // max-power: refused with the reason and the way into custom mode
        let (ok, text) = set(&CURVE);
        assert!(!ok, "{text}");
        assert!(
            text.contains(
                "the fan curve works in custom mode only, and the mode is now 'max-power'"
            ) && text
                .contains("write custom to /sys/class/platform-profile/platform-profile-0/profile"),
            "{text}"
        );

        enter_custom(m);
        // the shipped profile does not mark the table writes verified (todo-17): one plain state, no builder refs
        let (ok, text) = set(&CURVE);
        assert!(
            !ok && text.contains("the fan table and full speed are not available on this machine")
                && !text.contains("docs/design")
                && !text.contains("todo-"),
            "{text}"
        );
        m.verify_fan_calls();
        for (levels, why) in [
            (
                ["1", "1", "1", "1", "1", "1", "1", "1", "2", "5"],
                "point 9 is 2: the firmware minimum at this point is 3",
            ),
            (
                ["1", "1", "1", "1", "1", "1", "1", "1", "3", "11"],
                "point 10 is 11: a level is 1 to 10",
            ),
            (
                ["1", "1", "1", "3", "2", "3", "3", "3", "3", "5"],
                "point 5 is 2, lower than point 4 (3)",
            ),
        ] {
            let (ok, text) = set(&levels);
            assert!(!ok && text.contains(why), "{levels:?}: {text}");
        }
        let (ok, text) = set(&CURVE[..9]);
        assert!(!ok && text.contains("a fan curve has 10 levels"), "{text}");
        // the daemon checks too: a direct D-Bus call with a level 0 is refused
        let refused = stand_ins
            .call_method(
                Some("org.legionctl.System1"),
                "/org/legionctl/System1",
                Some("org.legionctl.System1"),
                "SetFanCurve",
                &(vec![0u8; 10],),
            )
            .await
            .unwrap_err();
        assert!(refused.to_string().contains("point 1 is 0"), "{refused}");
        assert_eq!(m.table(), [4; 10], "nothing reached the module");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_fan_curve_is_checked_written_point_by_point_read_back_and_written_again_after_resume()
     {
        let m = Machine::new("fancurve");
        m.set_table([4; 10]);
        let (stand_ins, asked, _processes) = start(&m).await;
        let op = on_bus(&m, "fancurve");
        refused_curves(&m, &op, &stand_ins).await;

        // the dry run (its flag before the levels) checks and shows the write, and writes nothing
        let (ok, text) = set(&op, &[&["--dry-run"][..], &CURVE].concat());
        assert!(
            ok && text.contains(
                "would set the fan curve to 1 1 2 2 3 4 5 6 8 10: write the changed points of pwm1_auto_point1_pwm"
            ),
            "{text}"
        );
        assert_eq!(m.table(), [4; 10]);

        // the write: points up from the top, then down from the bottom, each table between them valid; read back
        let (ok, text) = set(&op, &CURVE);
        assert!(ok, "{text}");
        assert!(
            text.contains(
                "fan curve is now 1 1 2 2 3 4 5 6 8 10 (was 4 4 4 4 4 4 4 4 4 4; writes "
            ),
            "{text}"
        );
        assert_eq!(replay(&text, [4; 10]), [1, 1, 2, 2, 3, 4, 5, 6, 8, 10]);
        assert_eq!(m.table(), [1, 1, 2, 2, 3, 4, 5, 6, 8, 10]);
        assert!(
            asked
                .lock()
                .unwrap()
                .iter()
                .any(|(_, action, _, _)| action == "org.legionctl.fan")
        );
        let status: Value = serde_json::from_str(&op.ok(&["fan", "status", "--json"])).unwrap();
        assert_eq!(
            status["curve"],
            serde_json::json!([1, 1, 2, 2, 3, 4, 5, 6, 8, 10])
        );
        assert_eq!(status["chosen"], status["curve"]);
        assert_eq!(status["mode"], "custom");
        let text = op.ok(&["fan", "status"]);
        assert!(text.contains("   9:  8  3500 / 3600 rpm"), "{text}");

        // suspend: the EC lost the table; after resume the daemon writes the user's curve again
        m.set_table([5; 10]);
        resume(&stand_ins).await;
        wait_for("the curve after resume", || {
            m.table() == [1, 1, 2, 2, 3, 4, 5, 6, 8, 10]
        });

        // reset: the profile's default curve, and no user curve
        let (ok, text) = run(&op, &["fan", "curve", "reset"]);
        assert!(ok, "{text}");
        assert_eq!(
            replay(&text, [1, 1, 2, 2, 3, 4, 5, 6, 8, 10]),
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
        );
        assert_eq!(m.table(), [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        let status: Value = serde_json::from_str(&op.ok(&["fan", "status", "--json"])).unwrap();
        assert_eq!(status["chosen"], Value::Null);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fan_max_mode_is_set_again_at_start_full_speed_follows_and_the_ec_lock_is_refused() {
        let m = Machine::new("fanmax");
        m.verify_fan_calls();
        let (stand_ins, _asked, (_bus, first)) = start(&m).await;
        let op = on_bus(&m, "fanmax");

        let (ok, text) = run(&op, &["fan", "max", "on"]);
        assert!(
            ok && text.contains("set fan_max_mode of hwmon legionctl_wmi to 1 (was 0)"),
            "{text}"
        );
        assert_eq!(m.hwmon_file("fan_max_mode"), "1");
        let status: Value = serde_json::from_str(&op.ok(&["fan", "status", "--json"])).unwrap();
        assert_eq!(status["max"], true);

        // after resume the module set it again itself: the daemon reads it and does not write
        resume_without_a_write_to(&m, &op, &stand_ins, "fan_max_mode").await;
        // a new module (the next start) has it off: the daemon sets it at start (system.db keeps it)
        m.set(&format!("{WMI_HWMON}/fan_max_mode"), "0");
        // the status is the module's state, not the kept choice
        let status: Value = serde_json::from_str(&op.ok(&["fan", "status", "--json"])).unwrap();
        assert_eq!(status["max"], false);
        drop(first);
        let _second = daemon(&m, &stand_ins).await;
        wait_for("fan max mode at start", || {
            m.hwmon_file("fan_max_mode") == "1"
        });

        let (ok, text) = run(&op, &["fan", "max", "off"]);
        assert!(ok, "{text}");
        assert_eq!(m.hwmon_file("fan_max_mode"), "0");
        // off stays off after resume, with no write
        resume_without_a_write_to(&m, &op, &stand_ins, "fan_max_mode").await;
        assert_eq!(
            m.file("/sys/devices/system/cpu/cpufreq/policy3/scaling_governor"),
            "performance"
        );

        // full speed: custom mode only; pwm1_enable 0 (full speed) and 2 (the table)
        let (ok, text) = run(&op, &["fan", "clean", "on"]);
        assert!(
            !ok && text.contains("full speed works in custom mode only"),
            "{text}"
        );
        assert_eq!(m.hwmon_file("pwm1_enable"), "2");
        enter_custom(&m);
        assert!(run(&op, &["fan", "clean", "on"]).0);
        assert_eq!(m.hwmon_file("pwm1_enable"), "0");
        let status: Value = serde_json::from_str(&op.ok(&["fan", "status", "--json"])).unwrap();
        assert_eq!(status["full_speed"], true);
        assert!(run(&op, &["fan", "clean", "off"]).0);
        assert_eq!(m.hwmon_file("pwm1_enable"), "2");

        // the EC fan lock: refused while the profile does not mark it verified; nothing opens /dev/port
        let (ok, text) = run(&op, &["fan", "lock", "on"]);
        assert!(
            !ok && text.contains("the EC fan lock is not available on this machine")
                && !text.contains("ec_fan_lock_verified"),
            "{text}"
        );
        let status = op.ok(&["fan", "status"]);
        assert!(
            status.contains("EC fan lock: not available on this machine"),
            "{status}"
        );
        let (ok, text) = run(&op, &["fan", "lock", "on", "--dry-run"]);
        assert!(ok, "{text}");
        assert!(
            text.contains(
                "would write EC RAM 0xC4AB: 0x08, through port I/O: 0x4e=0x2e 0x4f=0x11 0x4e=0x2f 0x4f=0xc4 \
                 0x4e=0x2e 0x4f=0x10 0x4e=0x2f 0x4f=0xab 0x4e=0x2e 0x4f=0x12 0x4e=0x2f 0x4f=0x08"
            ) && text.contains("warning: the EC fan lock is not available on this machine"),
            "{text}"
        );
        assert_eq!(m.file("/dev/port"), "<missing>");
    }

    /// No `legionctl_wmi` (not loaded, or the user went back to the upstream drivers): each fan command fails with
    /// the reason and the document, nothing is written, and the views say why the fans are unknown. No other path
    /// to the firmware is tried.
    #[tokio::test(flavor = "multi_thread")]
    async fn without_legionctl_wmi_each_fan_command_fails_with_the_reason() {
        const WHY: &str = "legionctl_wmi is not loaded: see docs/design/08-kernel.md";
        let m = Machine::new("nowmi");
        m.verify_fan_calls();
        std::fs::remove_dir_all(m.dir.join("root").join(&WMI_HWMON[1..])).unwrap();
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "nowmi");
        for args in [
            &["fan", "status"][..],
            &["fan", "max", "on"],
            &["fan", "max", "on", "--dry-run"],
            &["fan", "clean", "on"],
            &["fan", "curve", "reset"],
            &[&["fan", "curve", "set"][..], &CURVE].concat(),
            &[&["fan", "curve", "set", "--dry-run"][..], &CURVE].concat(),
        ] {
            let (ok, text) = run(&op, args);
            assert!(!ok && text.contains(WHY), "{args:?}: {text}");
        }
        let rows: Value = serde_json::from_str(&op.ok(&["sensors", "--json"])).unwrap();
        let fans = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "fan controls")
            .unwrap();
        assert!(fans["text"].as_str().unwrap().contains(WHY), "{fans}");
        let monitor: Value =
            serde_json::from_str(&op.ok(&["monitor", "--interval-ms", "100", "--json"])).unwrap();
        assert_eq!(monitor[0]["fan_rpm"], Value::Null, "{monitor}");
    }
}

/// `legionctl sensors` (todo-94) and the daemon's events for other tools (todo-95), on the recorded machine with the
/// daemon on its private bus.
mod sensors_and_events {
    use super::daemon_process::{Kill, start, wait_for};
    use super::operator_and_daemon::on_bus;
    use super::*;
    use futures_util::StreamExt;
    use serde_json::Value;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    /// A sysfs file that records each read: a FIFO whose writer gives `value` to each reader, then notes the read.
    fn watched(path: &Path, value: &'static str, reads: &Arc<Mutex<Vec<PathBuf>>>) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(path)
                .status()
                .unwrap()
                .success()
        );
        let (path, reads) = (path.to_path_buf(), reads.clone());
        std::thread::spawn(move || {
            loop {
                // the open waits for a reader
                std::fs::write(&path, value).unwrap();
                reads.lock().unwrap().push(path.clone());
            }
        });
    }

    fn row<'a>(rows: &'a Value, source: &str, name: &str) -> &'a Value {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["source"] == source && r["name"] == name)
            .unwrap_or_else(|| panic!("no row {source} / {name}: {rows:#}"))
    }

    /// CPU 0's MSR with `bits` in `IA32_PACKAGE_THERM_STATUS` (bit 0: thermal now, bit 1: thermal logged).
    fn msr(m: &Machine, bits: u8) {
        let msr = m.dir.join("root/dev/cpu/0/msr");
        std::fs::create_dir_all(msr.parent().unwrap()).unwrap();
        let mut bytes = vec![0u8; 0x1B1 + 8];
        bytes[0x1B1] = bits;
        // in one step: the daemon looks each second, and a half-written file reads as no MSR
        let new = msr.with_extension("new");
        std::fs::write(&new, bytes).unwrap();
        std::fs::rename(new, msr).unwrap();
    }

    /// hwmon device `n` of machine `m`: its name and files.
    fn hwmon(m: &Machine, n: u32, name: &str, files: &[(&str, &str)]) {
        m.set(&format!("/sys/class/hwmon/hwmon{n}/name"), name);
        for (file, value) in files {
            m.set(&format!("/sys/class/hwmon/hwmon{n}/{file}"), value);
        }
    }

    /// The sensors of machine `m`: coretemp, the battery, two `NVMe` drives and a DIMM on hwmon; the NVIDIA GPU asleep with a
    /// hwmon device on it whose reads are recorded, a `nvidia-smi` that logs each query, and the MSR's thermal bits.
    /// Returns the reads of the sleeping device's files and the `nvidia-smi` log.
    fn sensor_machine(m: &Machine) -> (Arc<Mutex<Vec<PathBuf>>>, PathBuf) {
        hwmon(
            m,
            0,
            "coretemp",
            &[
                ("temp1_input", "40000"),
                ("temp1_label", "Package id 0"),
                ("temp2_input", "51000"),
                ("temp1_crit", "100000"),
            ],
        );
        hwmon(
            m,
            1,
            "BAT0",
            &[("power1_input", "12500000"), ("in0_input", "16800")],
        );
        hwmon(
            m,
            2,
            "nvme",
            &[
                ("temp1_input", "38850"),
                ("temp1_label", "Composite"),
                ("curr1_input", "1500"),
                ("temp1_max", "84850"),
                ("device/model", "SKHynix_HFS001TEJ9X115N                 "),
            ],
        );
        // a second drive and a DIMM's SPD hub (I2C address 0x52: slot 2)
        hwmon(
            m,
            5,
            "nvme",
            &[
                ("temp1_input", "49850"),
                ("temp1_label", "Composite"),
                ("device/model", "CT4000P3PSSD8"),
            ],
        );
        hwmon(m, 6, "spd5118", &[("temp1_input", "56000")]);
        std::os::unix::fs::symlink(
            "../../../15-0052",
            m.dir.join("root/sys/class/hwmon/hwmon6/device"),
        )
        .unwrap();
        // the NVIDIA GPU sleeps; a hwmon device on it (nouveau's) must not be read, and nvidia-smi must not query it
        // (the AC policy's persistence mode call is not a query)
        let gpu = "/sys/bus/pci/devices/0000:01:00.0";
        m.set(&format!("{gpu}/vendor"), "0x10de");
        m.set(&format!("{gpu}/class"), "0x030000");
        m.set(&format!("{gpu}/power/runtime_status"), "suspended");
        hwmon(m, 3, "nouveau", &[]);
        std::os::unix::fs::symlink(
            "../../../bus/pci/devices/0000:01:00.0",
            m.dir.join("root/sys/class/hwmon/hwmon3/device"),
        )
        .unwrap();
        let reads = Arc::default();
        watched(
            &m.dir.join("root/sys/class/hwmon/hwmon3/temp1_input"),
            "45000\n",
            &reads,
        );
        let smi = m.dir.join("nvidia-smi.log");
        Machine::script(
            &m.dir.join("bin"),
            "nvidia-smi",
            &format!(
                "case \"$1\" in --query-gpu*) echo \"$@\" >> {};; esac",
                smi.display()
            ),
        );
        msr(m, 0b11);
        // an agent session of this user under memory pressure (its scope's cgroup PSI)
        let uid = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata("/proc/self").unwrap());
        m.set(
            &format!(
                "/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/\
                 legionctl-session-psi1.scope/memory.pressure"
            ),
            "some avg10=12.50 avg60=3.00 avg300=1.00 total=100\nfull avg10=4.00 avg60=1.00 avg300=0.00 total=50",
        );
        (reads, smi)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sensors_merge_every_source_with_units_and_ring_history_and_never_wake_a_sleeping_gpu()
    {
        let m = Machine::new("sensors");
        let (reads, smi) = sensor_machine(&m);
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let op = on_bus(&m, "sensors");
        let json = |args: &[&str]| -> Value {
            serde_json::from_str(&op.ok(&[args, &["--json"]].concat())).unwrap()
        };

        // a monitor sample goes into the ring; the next sensors view has it as history
        json(&["monitor", "--interval-ms", "100"]);
        m.set("/sys/class/hwmon/hwmon0/temp1_input", "70000");
        let rows = json(&["sensors"]);
        let package = row(&rows, "CPU", "Package id 0");
        assert_eq!(
            (&package["value"], &package["unit"], &package["history"]),
            (
                &serde_json::json!(70.0),
                &serde_json::json!("°C"),
                &serde_json::json!([40.0, 70.0])
            ),
            "{rows:#}"
        );
        for (source, name, value, unit) in [
            ("CPU", "temp2", 51.0, "°C"),
            ("battery", "power1", 12.5, "W"),
            ("battery", "in0", 16.8, "V"),
            ("NVMe SKHynix HFS001TEJ9X115N", "Composite", 38.85, "°C"),
            ("NVMe CT4000P3PSSD8", "Composite", 49.85, "°C"),
            ("memory DIMM 2", "temp1", 56.0, "°C"),
            ("NVMe SKHynix HFS001TEJ9X115N", "curr1", 1.5, "A"),
            // the fan RPM from the recorded legionctl_wmi hwmon device
            ("fans", "fan1", 5100.0, "RPM"),
            ("fans", "fan2", 5100.0, "RPM"),
            (
                "cgroup PSI",
                "memory pressure of session psi1, some avg10",
                12.5,
                "%",
            ),
        ] {
            let r = row(&rows, source, name);
            assert_eq!(
                (&r["value"], &r["unit"]),
                (&serde_json::json!(value), &serde_json::json!(unit)),
                "{r}"
            );
        }
        // limits are not readings
        let hwmon_rows = |rows: &Value, device: &str| {
            rows.as_array()
                .unwrap()
                .iter()
                .filter(|r| r["source"] == device)
                .count()
        };
        // each drive by its model: two drives are two sources
        assert_eq!(
            hwmon_rows(&rows, "NVMe SKHynix HFS001TEJ9X115N"),
            2,
            "{rows:#}"
        );
        assert_eq!(hwmon_rows(&rows, "NVMe CT4000P3PSSD8"), 1, "{rows:#}");
        assert_eq!(hwmon_rows(&rows, "CPU"), 2, "{rows:#}");
        // the sleeping GPU: its state only; the MSR through the daemon, the fan controls, the power source
        for (source, name, text) in [
            ("PCI runtime PM", "NVIDIA GPU", "suspended"),
            (
                "GPU (nouveau)",
                "device",
                "asleep, not read (a read wakes it)",
            ),
            ("MSR", "throttle reasons now", "thermal"),
            ("legionctl-system", "firmware mode", "max-power"),
            ("legionctl-system", "fan max mode", "off"),
            ("legionctl-system", "fan full speed", "off"),
            ("legionctl-system", "fan table", "1 2 3 4 5 6 7 8 9 10"),
            ("power_supply", "power source", "ac"),
            ("eBPF workload", "workload", "none"),
        ] {
            assert_eq!(row(&rows, source, name)["text"], text, "{rows:#}");
        }
        assert_eq!(hwmon_rows(&rows, "nvidia-smi"), 0, "{rows:#}");
        assert!(!smi.exists(), "nvidia-smi queried the sleeping GPU");
        assert_eq!(
            *reads.lock().unwrap(),
            Vec::<PathBuf>::new(),
            "a sleeping device's sensor was read"
        );

        fans_follow_the_module(&m, &op);

        // the text groups by source, with the range of the history
        let text = op.ok(&["sensors"]);
        assert!(text.contains("CPU:\n  Package id 0"), "{text}");
        assert!(text.contains("(40.0-70.0 in 5 samples)"), "{text}");

        // --watch: a view each second, each with one more sample of history
        assert_eq!(watched_histories(&op), (6, 7));

        // the same channels in OpenMetrics
        let metrics = metrics(&op);
        for line in [
            "legionctl_hwmon_celsius{device=\"hwmon0 coretemp\",sensor=\"Package id 0\"} 70",
            "legionctl_hwmon_watts{device=\"hwmon1 BAT0\",sensor=\"power1\"} 12.5",
            "legionctl_hwmon_rpm{device=\"hwmon9 legionctl_wmi\",sensor=\"fan1\"} 2200",
            "legionctl_throttle_active{reason=\"thermal\"} 1",
            "legionctl_session_memory_pressure_some_percent{session=\"psi1\"} 12.5",
        ] {
            assert!(metrics.contains(line), "{line}\n{metrics}");
        }
        assert_eq!(*reads.lock().unwrap(), Vec::<PathBuf>::new());
    }

    /// The fans speed up: the next view reads the module's hwmon device again, and the daemon's fan RPM (monitor,
    /// bench) is the module's too.
    fn fans_follow_the_module(m: &Machine, op: &Operator) {
        let json = |args: &[&str]| -> Value {
            serde_json::from_str(&op.ok(&[args, &["--json"]].concat())).unwrap()
        };
        m.set(&format!("{WMI_HWMON}/fan1_input"), "2200");
        let rows = json(&["sensors"]);
        assert_eq!(row(&rows, "fans", "fan1")["value"], 2200.0);
        let monitor = json(&["monitor", "--interval-ms", "100"]);
        assert_eq!(
            monitor[0]["fan_rpm"],
            serde_json::json!({"fan1": 2200, "fan2": 5100})
        );
    }

    /// The history length of coretemp's package temperature in the first two views of `sensors --watch --json`.
    fn watched_histories(op: &Operator) -> (usize, usize) {
        let mut watch = Kill::spawn(
            std::process::Command::new(env!("CARGO_BIN_EXE_legionctl"))
                .args(["sensors", "--watch", "--json"])
                .env_clear()
                .env("XDG_RUNTIME_DIR", &op.runtime)
                .stdout(std::process::Stdio::piped()),
        );
        let mut lines =
            std::io::BufRead::lines(std::io::BufReader::new(watch.stdout.take().unwrap()));
        let mut history = || {
            let view: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
            row(&view, "CPU", "Package id 0")["history"]
                .as_array()
                .unwrap()
                .len()
        };
        (history(), history())
    }

    /// `GET /v1/metrics` of the operator.
    fn metrics(op: &Operator) -> String {
        let token = std::fs::read_to_string(op.runtime.join(legionctl_api::USER_TOKEN)).unwrap();
        let mut stream =
            std::os::unix::net::UnixStream::connect(op.runtime.join("legionctl/api.sock")).unwrap();
        std::io::Write::write_all(
            &mut stream,
            format!("GET /v1/metrics HTTP/1.1\r\nhost: l\r\nconnection: close\r\nauthorization: Bearer {token}\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
        let mut metrics = String::new();
        std::io::Read::read_to_string(&mut stream, &mut metrics).unwrap();
        metrics
    }

    /// The journal's native socket, a stand-in in the machine's tree: each entry as its fields.
    fn journal(m: &Machine) -> Arc<Mutex<Vec<BTreeMap<String, String>>>> {
        let path = m.dir.join("root/run/systemd/journal/socket");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let socket = std::os::unix::net::UnixDatagram::bind(&path).unwrap();
        let entries = Arc::<Mutex<Vec<BTreeMap<String, String>>>>::default();
        let kept = entries.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 65536];
            while let Ok(n) = socket.recv(&mut buf) {
                let entry = String::from_utf8_lossy(&buf[..n])
                    .lines()
                    .filter_map(|l| l.split_once('='))
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();
                kept.lock().unwrap().push(entry);
            }
        });
        entries
    }

    /// The signals other tools follow, as (member, arguments), from now on.
    async fn collect(system: &zbus::Proxy<'static>) -> Arc<Mutex<Vec<(String, Value)>>> {
        let signals = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let mut stream = system.receive_all_signals().await.unwrap();
        let kept = signals.clone();
        tokio::spawn(async move {
            while let Some(s) = stream.next().await {
                let member = s.header().member().unwrap().to_string();
                let body = match member.as_str() {
                    "PowerSourceChanged" => {
                        serde_json::json!(s.body().deserialize::<(String,)>().unwrap().0)
                    }
                    "WorkloadChanged" | "ThrottleChanged" => {
                        serde_json::json!(s.body().deserialize::<(Vec<String>,)>().unwrap().0)
                    }
                    "ModeChanged" => {
                        serde_json::json!(s.body().deserialize::<(String, bool, String)>().unwrap())
                    }
                    "FanModeChanged" => {
                        serde_json::json!(s.body().deserialize::<(bool, bool)>().unwrap())
                    }
                    _ => continue,
                };
                kept.lock().unwrap().push((member, body));
            }
        });
        signals
    }

    /// One journal entry per signal, in the same order, with the fields of each event; the end of throttling comes
    /// after it held (todo-126).
    fn assert_journal(entries: &[BTreeMap<String, String>]) {
        let event = |e: &BTreeMap<String, String>, keys: &[&str]| -> Vec<String> {
            keys.iter()
                .map(|k| e.get(*k).cloned().unwrap_or_else(|| format!("<no {k}>")))
                .collect()
        };
        let all: Vec<Vec<String>> = entries
            .iter()
            .map(|e| event(e, &["SYSLOG_IDENTIFIER", "LEGIONCTL_EVENT", "MESSAGE"]))
            .collect();
        assert_eq!(
            all,
            [
                ["legionctl-system", "workload", "workload build"],
                [
                    "legionctl-system",
                    "mode",
                    "mode balanced (default on battery)"
                ],
                ["legionctl-system", "power-source", "power source battery"],
                ["legionctl-system", "mode", "mode performance (Fn+Q choice)"],
                [
                    "legionctl-system",
                    "throttle",
                    "throttling: thermal (changes since the last entry: 1)"
                ],
                [
                    "legionctl-system",
                    "fan-mode",
                    "fan max mode on, full speed off"
                ],
                [
                    "legionctl-system",
                    "throttle",
                    "throttling stopped (changes since the last entry: 1)"
                ],
            ],
            "{entries:#?}"
        );
        assert_eq!(event(&entries[0], &["LEGIONCTL_WORKLOAD"]), ["build"]);
        assert_eq!(event(&entries[2], &["LEGIONCTL_SOURCE"]), ["battery"]);
        assert_eq!(
            event(
                &entries[3],
                &[
                    "LEGIONCTL_MODE",
                    "LEGIONCTL_FN_Q_CHOICE",
                    "LEGIONCTL_SOURCE"
                ]
            ),
            ["performance", "1", "battery"]
        );
        assert_eq!(
            event(&entries[4], &["LEGIONCTL_REASONS", "LEGIONCTL_WORKLOAD"]),
            ["thermal", "build"]
        );
        assert_eq!(
            event(
                &entries[5],
                &["LEGIONCTL_FAN_MAX", "LEGIONCTL_FAN_FULL_SPEED"]
            ),
            ["on", "off"]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn each_change_other_tools_follow_is_one_signal_and_one_journal_entry() {
        let m = Machine::new("events-out");
        let entries = journal(&m);
        let (stand_ins, _asked, _processes) = start(&m).await;
        let system = zbus::Proxy::new(
            &stand_ins,
            "org.legionctl.System1",
            "/org/legionctl/System1",
            "org.legionctl.System1",
        )
        .await
        .unwrap();
        let signals = collect(&system).await;
        let resume = async || {
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
        };
        let wait = |n: usize| {
            wait_for(&format!("{n} signals"), || {
                signals.lock().unwrap().len() >= n
            });
        };

        // a workload starts (the eBPF sensor writes the kinds; as root in CI it does, here the test does, after the
        // sensor that cannot load wrote why at start)
        wait_for("the workload sensor's start", || {
            m.file("/run/legionctl/workload.error") != "<missing>"
        });
        m.set("/run/legionctl/workload", "build");
        resume().await;
        wait(1);
        // the AC adapter is unplugged: the battery policy changes the mode too
        m.set("/sys/class/power_supply/ADP0/online", "0");
        resume().await;
        wait(3);
        // Fn+Q to performance: the firmware's choice, which the policy keeps (the choice first: a look between the two
        // writes, such as a resume still in the queue, sees no change, not the mode without its choice)
        m.record(Key::Override, "performance");
        m.set(PROFILE_PATH, "performance");
        resume().await;
        wait(4);
        // throttling starts during the build (the MSR's thermal status bit, seen at an event) and is counted with it
        msr(&m, 0b11);
        resume().await;
        wait(5);
        let sample: String = system.call("Sample", &()).await.unwrap();
        let sample: Value = serde_json::from_str(&sample).unwrap();
        assert_eq!(
            sample["throttle_episodes"],
            serde_json::json!({"thermal": {"build": 1}}),
            "{sample} {:?}",
            signals.lock().unwrap()
        );
        // it stops with no event: the daemon looks each second while throttled (not at the 30 s tick)
        msr(&m, 0b10);
        wait(6);
        // fan max mode through the daemon's method: announced as the module reports it
        let _: Vec<String> = system.call("SetFanMax", &(true,)).await.unwrap();
        assert_eq!(m.hwmon_file("fan_max_mode"), "1");
        wait(7);
        // one more resume changes nothing that other tools follow: nothing more is announced
        resume().await;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        wait_for("the end of throttling in the journal", || {
            entries.lock().unwrap().len() >= 7
        });

        let signals = signals.lock().unwrap().clone();
        let shown: Vec<(&str, &Value)> = signals.iter().map(|(m, b)| (m.as_str(), b)).collect();
        assert_eq!(
            shown,
            [
                ("WorkloadChanged", &serde_json::json!(["build"])),
                (
                    "ModeChanged",
                    &serde_json::json!(["balanced", false, "battery"])
                ),
                ("PowerSourceChanged", &serde_json::json!("battery")),
                (
                    "ModeChanged",
                    &serde_json::json!(["performance", true, "battery"])
                ),
                ("ThrottleChanged", &serde_json::json!(["thermal"])),
                ("ThrottleChanged", &serde_json::json!([])),
                ("FanModeChanged", &serde_json::json!([true, false])),
            ],
            "{signals:?}"
        );
        assert_journal(&entries.lock().unwrap());
    }

    /// The adapter is unplugged and its udev event comes late: the daemon's next look (here the look each second while
    /// throttled) applies the battery policy before it announces, so no tool hears the AC mode as the battery default.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_power_source_change_without_its_event_is_announced_after_its_policy() {
        let m = Machine::new("late-source");
        let entries = journal(&m);
        let (stand_ins, _asked, _processes) = start(&m).await;
        let system = zbus::Proxy::new(
            &stand_ins,
            "org.legionctl.System1",
            "/org/legionctl/System1",
            "org.legionctl.System1",
        )
        .await
        .unwrap();
        let signals = collect(&system).await;
        // throttling, seen at a resume: from then on the daemon looks each second
        msr(&m, 0b11);
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
        wait_for("the throttle start", || signals.lock().unwrap().len() == 1);
        assert_eq!(m.file(PROFILE_PATH), "max-power");

        m.set("/sys/class/power_supply/ADP0/online", "0");
        wait_for("the power source change and the mode", || {
            signals.lock().unwrap().len() == 3 && entries.lock().unwrap().len() == 3
        });
        let signals = signals.lock().unwrap().clone();
        let shown: Vec<(&str, &Value)> = signals.iter().map(|(m, b)| (m.as_str(), b)).collect();
        assert_eq!(
            shown[1..],
            [
                (
                    "ModeChanged",
                    &serde_json::json!(["balanced", false, "battery"])
                ),
                ("PowerSourceChanged", &serde_json::json!("battery")),
            ],
            "{signals:?}"
        );
        assert_eq!(m.file(PROFILE_PATH), "balanced");
        assert_eq!(
            entries.lock().unwrap()[1]
                .get("MESSAGE")
                .map(String::as_str),
            Some("mode balanced (default on battery)")
        );
    }

    /// Throttling that starts and stops at each look (a build that keeps the CPU at its thermal limit, todo-126): each
    /// change is still a signal and an episode, but the journal gets the start and, once it held, the end, with the
    /// number of changes between them.
    #[tokio::test(flavor = "multi_thread")]
    async fn throttling_that_flaps_gives_two_journal_entries_and_every_signal() {
        let m = Machine::new("throttle-flaps");
        msr(&m, 0b00);
        let entries = journal(&m);
        let (stand_ins, _asked, _processes) = start(&m).await;
        let system = zbus::Proxy::new(
            &stand_ins,
            "org.legionctl.System1",
            "/org/legionctl/System1",
            "org.legionctl.System1",
        )
        .await
        .unwrap();
        let signals = collect(&system).await;
        for n in 1..=24 {
            msr(&m, if n % 2 == 1 { 0b11 } else { 0b10 });
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
            wait_for(&format!("throttle change {n}"), || {
                signals.lock().unwrap().len() == n
            });
        }
        let throttle = || -> Vec<(String, String)> {
            entries
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e["LEGIONCTL_EVENT"] == "throttle")
                .map(|e| {
                    let field = |k: &str| e.get(k).cloned().unwrap_or_default();
                    (field("MESSAGE"), field("LEGIONCTL_CHANGES"))
                })
                .collect()
        };
        wait_for("the end of throttling in the journal", || {
            throttle().len() >= 2
        });
        // nothing more comes after the end held
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let throttle = throttle();
        assert_eq!(
            throttle,
            [
                (
                    "throttling: thermal (changes since the last entry: 1)".into(),
                    "1".into()
                ),
                (
                    "throttling stopped (changes since the last entry: 23)".into(),
                    "23".into()
                ),
            ]
        );
        assert_eq!(signals.lock().unwrap().len(), 24);
        let sample: String = system.call("Sample", &()).await.unwrap();
        let sample: Value = serde_json::from_str(&sample).unwrap();
        assert_eq!(
            sample["throttle_episodes"],
            serde_json::json!({"thermal": {"none": 12}})
        );
    }
}

/// Windows parity in user space (todo-82 part 3): the NVIDIA GPU's sleep, keyboard RGB through `OpenRGB`, fwupd and the
/// vitals overlay, each through the real operator (and the real daemon on a private bus for root writes).
mod parity {
    use super::daemon_process::{start, wait_for};
    use super::operator_and_daemon::{on_bus, pci_machine};
    use super::*;
    use serde_json::Value;
    use std::ffi::OsString;
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    const CONTROL: &str = "/sys/bus/pci/devices/0000:01:00.0/power/control";

    fn refused(out: &Output, why: &str) {
        let text = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success() && text.contains(why),
            "{why} missing:\n{text}"
        );
    }

    /// The NVIDIA GPU's sleep (`gpu power`): status names what keeps it awake, doctor finds a GPU that never sleeps,
    /// off is refused before polkit while gnome-shell holds the GPU, the dry run changes nothing, the daemon writes
    /// power/control after polkit, undo powers it again, and a GPU on vfio-pci is its owner's.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_nvidia_gpu_sleeps_only_when_no_program_uses_it_and_undo_keeps_it_powered_again() {
        let m = Machine::new("gpupower");
        let _kernel = pci_machine(&m);
        m.set(CONTROL, "on");
        m.set(
            "/sys/bus/pci/devices/0000:01:00.0/power/runtime_status",
            "active",
        );
        let (_stand_ins, asked, _processes) = start(&m).await;
        let op = on_bus(&m, "gpupower");
        let asked_power = || {
            asked
                .lock()
                .unwrap()
                .iter()
                .filter(|a| a.1 == "org.legionctl.gpu.power")
                .count()
        };

        let status = op.ok(&["gpu", "status"]);
        assert!(
            status.contains("NVIDIA GPU power: active (power/control on)")
                && status.contains("keeps it awake: gnome-shell (pid 4242) uses /dev/dri/card0"),
            "{status}"
        );
        let doctor = String::from_utf8_lossy(&op.cli(&["doctor"]).stdout).to_string();
        assert!(doctor.contains("the NVIDIA GPU never sleeps"), "{doctor}");

        refused(&op.cli(&["gpu", "power", "off"]), "gnome-shell (pid 4242)");
        assert_eq!(m.file(CONTROL), "on");
        assert_eq!(asked_power(), 0, "refused before the polkit dialog");

        std::fs::remove_file(m.dir.join("root/proc/4242/fd/7")).unwrap();
        let plan = op.ok(&["gpu", "power", "off", "--dry-run"]);
        assert!(
            plan.contains(&format!("would write {CONTROL}: auto")),
            "{plan}"
        );
        assert_eq!(m.file(CONTROL), "on", "a dry run changes nothing");

        let done = op.ok(&["gpu", "power", "off"]);
        assert!(
            done.contains(&format!("wrote {CONTROL}: auto (was on)")),
            "{done}"
        );
        assert_eq!(m.file(CONTROL), "auto");
        assert_eq!(asked_power(), 1);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let event = loop {
            let events: Value = serde_json::from_str(&op.ok(&["events", "--json"])).unwrap();
            if let Some(e) = events["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["kind"] == "machine.set-gpu-power")
            {
                break e["id"].to_string();
            }
            assert!(std::time::Instant::now() < deadline, "not recorded");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        op.ok(&["undo", &event]);
        assert_eq!(m.file(CONTROL), "on", "undone");

        let vfio = m.dir.join("root/sys/bus/pci/devices/0000:01:00.0/driver");
        std::fs::remove_file(&vfio).unwrap();
        std::os::unix::fs::symlink("../../drivers/vfio-pci", &vfio).unwrap();
        refused(
            &op.cli(&["gpu", "power", "on"]),
            "its owner manages its power",
        );
    }

    /// `OpenRGB`'s SDK server as `OpenRGB` 1.0 answers it: protocol 4, one controller (this keyboard), two saved
    /// profiles; each loaded profile goes into the returned list.
    fn openrgb(m: &Machine) -> Arc<Mutex<Vec<String>>> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        std::fs::write(
            m.dir.join("openrgb.addr"),
            listener.local_addr().unwrap().to_string(),
        )
        .unwrap();
        let loaded = Arc::new(Mutex::new(Vec::new()));
        let log = loaded.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut s = stream.unwrap();
                let log = log.clone();
                std::thread::spawn(move || {
                    let mut header = [0u8; 16];
                    while s.read_exact(&mut header).is_ok() {
                        let word =
                            |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
                        let (id, size) = (word(8), word(12));
                        let mut data = vec![0u8; size as usize];
                        s.read_exact(&mut data).unwrap();
                        let string = |text: &str| {
                            let mut b = u16::try_from(text.len() + 1)
                                .unwrap()
                                .to_le_bytes()
                                .to_vec();
                            b.extend(text.as_bytes());
                            b.push(0);
                            b
                        };
                        let reply: Option<Vec<u8>> = match id {
                            40 => Some(4u32.to_le_bytes().to_vec()),
                            0 => Some(1u32.to_le_bytes().to_vec()),
                            1 => {
                                let mut d = vec![0u8; 8];
                                d[4] = 5; // keyboard
                                d.extend(string("Lenovo Legion 7 Gen 8"));
                                d.extend(string("Lenovo"));
                                Some(d)
                            }
                            150 => {
                                let mut d = vec![0u8; 4];
                                d.extend(2u16.to_le_bytes());
                                d.extend(string("ac-wave"));
                                d.extend(string("battery-dim"));
                                Some(d)
                            }
                            152 => {
                                let name = String::from_utf8(data).unwrap();
                                log.lock()
                                    .unwrap()
                                    .push(name.trim_end_matches('\0').to_string());
                                None
                            }
                            _ => None,
                        };
                        if let Some(d) = reply {
                            // a notice first, as OpenRGB sends one when its device list changes
                            let mut p = b"ORGB".to_vec();
                            for n in [0u32, 100, 0] {
                                p.extend(n.to_le_bytes());
                            }
                            p.extend(b"ORGB");
                            for n in [0u32, id, u32::try_from(d.len()).unwrap()] {
                                p.extend(n.to_le_bytes());
                            }
                            p.extend(d);
                            s.write_all(&p).unwrap();
                        }
                    }
                });
            }
        });
        loaded
    }

    /// A power-profiles-daemon stand-in: its `ActiveProfile` property, read and written over D-Bus.
    struct Ppd(Arc<Mutex<String>>);

    #[zbus::interface(name = "org.freedesktop.UPower.PowerProfiles")]
    impl Ppd {
        #[zbus(property)]
        fn active_profile(&self) -> String {
            self.0.lock().unwrap().clone()
        }

        #[zbus(property)]
        fn set_active_profile(&mut self, profile: String) {
            *self.0.lock().unwrap() = profile;
        }
    }

    /// power-profiles-daemon over D-Bus, not `powerprofilesctl` (a Python program that aborted at exit under Python
    /// 3.14 each time the daemon ran it): the dry run names the profile the policy wants and sets nothing, the apply
    /// sets it through the daemon's property, and an installed daemon that is not on the bus fails the apply with the
    /// cause, not "no power-profiles-daemon".
    #[tokio::test(flavor = "multi_thread")]
    async fn the_power_profile_is_set_over_d_bus_and_a_daemon_that_does_not_answer_fails_the_apply()
    {
        let m = Machine::new("ppd");
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let profile = Arc::new(Mutex::new("performance".to_string()));
        let ppd = zbus::connection::Builder::address(
            format!("unix:path={}", m.dir.join("bus").display()).as_str(),
        )
        .unwrap()
        .name("org.freedesktop.UPower.PowerProfiles")
        .unwrap()
        .serve_at(
            "/org/freedesktop/UPower/PowerProfiles",
            Ppd(profile.clone()),
        )
        .unwrap()
        .build()
        .await
        .unwrap();
        // installed after the daemon's start, so its first apply does not use the stand-in
        m.set(
            "/usr/share/dbus-1/system-services/org.freedesktop.UPower.PowerProfiles.service",
            "[D-BUS Service]",
        );
        let op = on_bus(&m, "ppd");

        let plan = op.ok(&["power", "apply", "--source", "battery", "--dry-run"]);
        let wanted = plan
            .lines()
            .find_map(|l| l.strip_prefix("would set the power-profiles-daemon profile "))
            .unwrap_or_else(|| panic!("no power-profiles-daemon step: {plan}"))
            .to_string();
        assert_ne!(wanted, "performance");
        assert_eq!(
            *profile.lock().unwrap(),
            "performance",
            "a dry run sets nothing"
        );
        let applied = op.ok(&["power", "apply", "--source", "battery"]);
        assert!(
            applied.contains(&format!("set the power-profiles-daemon profile {wanted}")),
            "{applied}"
        );
        assert_eq!(*profile.lock().unwrap(), wanted);

        drop(ppd);
        let failed = op.cli(&["power", "apply", "--source", "ac"]);
        let why = String::from_utf8_lossy(&failed.stderr);
        assert!(
            !failed.status.success() && why.contains("reading the power-profiles-daemon profile"),
            "{why}"
        );
    }

    /// Keyboard RGB (`OpenRGB`, knob rgb.profile): `rgb status` finds the keyboard on USB and in `OpenRGB` with its
    /// profiles; the daemon loads the profile of the power source's layer at start and when the source changes (once,
    /// not again for the same profile), the dry run names it, `knobs show` has it, and a profile `OpenRGB` does not have
    /// is a warning with nothing loaded.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_keyboard_light_follows_the_power_source_through_openrgb_profiles() {
        let m = Machine::new("rgb");
        for (file, value) in [("idVendor", "048d"), ("idProduct", "c988")] {
            m.set(&format!("/sys/bus/usb/devices/1-3/{file}"), value);
        }
        let layer = |source: &str, profile: &str| {
            m.set(
                &format!("/etc/legionctl/configsets/50-rgb-{source}.jsonc"),
                &format!(
                    r#"{{"when": {{"power": "{source}"}}, "set": {{"rgb.profile": "{profile}"}}}}"#
                ),
            );
        };
        layer("ac", "ac-wave");
        layer("battery", "battery-dim");
        let loaded = openrgb(&m);
        let (_stand_ins, _asked, _processes) = start(&m).await;
        wait_for("the AC profile at start", || {
            loaded.lock().unwrap().as_slice() == ["ac-wave"]
        });
        let address = std::fs::read_to_string(m.dir.join("openrgb.addr")).unwrap();
        let op = Operator::start(
            "rgb",
            &[
                ("PATH", m.dir.join("bin").into()),
                ("HOME", m.dir.join("home").into()),
                ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
                (
                    "DBUS_SYSTEM_BUS_ADDRESS",
                    format!("unix:path={}", m.dir.join("bus").display()).into(),
                ),
                ("LEGIONCTL_OPENRGB", OsString::from(&address)),
            ],
        );

        let status = op.ok(&["rgb", "status"]);
        assert!(
            status.contains("device: Lenovo Legion 7 Gen 8")
                && status.contains("profile: battery-dim")
                && status.contains("SDK protocol 4")
                && !status.contains("problem"),
            "{status}"
        );

        // still on AC: the daemon's own reaction to an unplug would race the calls below
        let plan = op.ok(&["power", "apply", "--source", "battery", "--dry-run"]);
        assert!(
            plan.contains("would load the OpenRGB profile battery-dim"),
            "{plan}"
        );
        assert_eq!(loaded.lock().unwrap().len(), 1, "a dry run loads nothing");

        let applied = op.ok(&["power", "apply", "--source", "battery"]);
        assert!(
            applied.contains("loaded the OpenRGB profile battery-dim (was ac-wave)"),
            "{applied}"
        );
        op.ok(&["power", "apply", "--source", "battery"]);
        assert_eq!(
            loaded.lock().unwrap().as_slice(),
            ["ac-wave", "battery-dim"],
            "the same profile is not loaded again"
        );
        let knob: Value =
            serde_json::from_str(&op.ok(&["knobs", "show", "rgb.profile", "--json"])).unwrap();
        assert_eq!(knob["knob"]["value"], "battery-dim");
        // the machine is on AC: the AC layer's profile is what the policy wants at the next event
        assert_eq!(knob["knob"]["desired"], "ac-wave");

        // a step that fails fails the apply (todo-142 F-5): exit 1 with the step and its reason, not a warning line
        layer("battery", "missing");
        let missing = op.cli(&["power", "apply", "--source", "battery"]);
        let why = String::from_utf8_lossy(&missing.stderr);
        assert!(!missing.status.success(), "{why}");
        assert!(
            why.contains("load the OpenRGB profile missing failed:")
                && why.contains("OpenRGB has no profile missing")
                && why.contains("; done before it: "),
            "{why}"
        );
        // the daemon also replans on the layer change (on AC: ac-wave again), but never loads a missing profile
        assert!(!loaded.lock().unwrap().iter().any(|p| p == "missing"));

        std::fs::remove_dir_all(m.dir.join("root/sys/bus/usb/devices/1-3")).unwrap();
        let out = op.cli(&["rgb", "status"]);
        assert!(!out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("not on the USB bus"),
            "{out:?}"
        );
    }

    /// fwupd (`firmware status`, `firmware update`): without fwupd the status says so and an update is refused; a
    /// stand-in fwupdmgr with four devices; the status shows the
    /// upgrade of each, the dry run names the versions and the exact command and runs nothing, a device with no
    /// upgrade and a name of two devices are refused, and the update runs fwupdmgr without a restart and records the
    /// old version.
    #[test]
    fn firmware_updates_come_from_fwupd_with_a_dry_run_first() {
        let m = Machine::new("fwupd");
        // no fwupd: its state, not a failure; an update needs it
        let missing = m.ok(&["firmware", "status"]);
        assert_eq!(
            missing.trim(),
            "fwupd is not installed: install fwupd (pacman -S fwupd)"
        );
        refused(&m.legion(&["firmware", "update", "aa11"]), "install fwupd");
        let log = m.dir.join("fwupdmgr.log");
        let devices = r#"{"Devices": [
          {"Name": "SK hynix PC801", "DeviceId": "aa11", "Version": "1.0", "Flags": ["internal", "updatable"],
           "Releases": [{"Version": "1.0", "Flags": []}, {"Version": "1.2", "Summary": "Fixes resume", "Flags": ["is-upgrade"]}]},
          {"Name": "Logi Bolt Receiver", "DeviceId": "bb22", "Version": "MBO04.00", "Flags": ["updatable"], "Releases": []},
          {"Name": "Logi Bolt Receiver", "DeviceId": "dd44", "Version": "MBO04.00", "Flags": ["updatable"], "Releases": []},
          {"Name": "UEFI dbx", "DeviceId": "cc33", "Version": "371", "Flags": []}
        ]}"#;
        Machine::script(
            &m.dir.join("bin"),
            "fwupdmgr",
            &format!(
                "echo \"$*\" >> {}; case $1 in get-devices) echo '{devices}';; update) echo 'Successfully installed firmware';; esac",
                log.display()
            ),
        );
        let status = m.ok(&["firmware", "status"]);
        assert!(
            status.contains("SK hynix PC801 1.0 - update to 1.2: Fixes resume")
                && status.contains("Logi Bolt Receiver MBO04.00 - no update")
                && status.contains("UEFI dbx 371 - fwupd cannot update it"),
            "{status}"
        );

        let plan = m.ok(&["firmware", "update", "SK hynix PC801", "--dry-run"]);
        assert!(
            plan.contains("would update SK hynix PC801: 1.0 -> 1.2")
                && plan.contains("would run: fwupdmgr update aa11 --assume-yes --no-reboot-check"),
            "{plan}"
        );
        refused(
            &m.legion(&["firmware", "update", "bb22", "--dry-run"]),
            "fwupd has no upgrade for it",
        );
        refused(
            &m.legion(&["firmware", "update", "zz99"]),
            "fwupd has no device zz99",
        );
        // two receivers of one model: the name is not enough (R3-31)
        refused(
            &m.legion(&["firmware", "update", "Logi Bolt Receiver", "--dry-run"]),
            "2 devices are named Logi Bolt Receiver: give the device id (bb22, dd44)",
        );
        assert!(
            !std::fs::read_to_string(&log).unwrap().contains("update"),
            "nothing updated yet"
        );

        let done = m.ok(&["firmware", "update", "aa11"]);
        assert!(
            done.contains("updated SK hynix PC801: 1.0 -> 1.2"),
            "{done}"
        );
        assert!(
            std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .any(|l| l == "update aa11 --assume-yes --no-reboot-check"),
            "fwupdmgr ran the update without a restart"
        );
        let events: Value = serde_json::from_str(&m.ok(&["events", "--json"])).unwrap();
        let event = events["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == "firmware.change")
            .expect("the update is recorded with its versions");
        assert_eq!(event["payload"]["from"], "1.0");
        assert_eq!(event["payload"]["to"], "1.2");
    }

    #[derive(Clone, Default)]
    struct Osd(Arc<Mutex<Vec<(String, String)>>>);

    #[zbus::interface(name = "org.legionctl.Osd")]
    impl Osd {
        fn show(&self, kind: &str, value: &str, detail: &str, level: f64) {
            // the overlay sends no detail and no level; the interface has them for the other kinds
            let _ = (detail, level);
            self.0.lock().unwrap().push((kind.into(), value.into()));
        }
    }

    /// The vitals overlay (`overlay on|off`): the operator sends the key readings to the extension every 2 s (CPU
    /// package temperature and power, the GPU asleep or its temperature and power, the fans, the firmware mode and
    /// the power source), and an empty value once after off; nothing while it is off.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_vitals_overlay_shows_the_key_readings_until_it_is_turned_off() {
        let m = Machine::new("vitals");
        let _kernel = pci_machine(&m);
        let gpu = "/sys/bus/pci/devices/0000:01:00.0/power/runtime_status";
        m.set(gpu, "suspended");
        m.set("/sys/class/hwmon/hwmon8/name", "coretemp");
        m.set("/sys/class/hwmon/hwmon8/temp1_input", "71000");
        m.set("/sys/class/hwmon/hwmon8/temp1_label", "Package id 0");
        // the fans: the recorded legionctl_wmi device
        Machine::script(&m.dir.join("bin"), "nvidia-smi", "echo '42.4, 51, 3, 1200'");
        let (_stand_ins, _asked, _processes) = start(&m).await;
        let address = format!("unix:path={}", m.dir.join("bus").display());
        let shown = Osd::default();
        let _extension = zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .name("org.legionctl.Osd")
            .unwrap()
            .serve_at("/org/legionctl/Osd", shown.clone())
            .unwrap()
            .build()
            .await
            .unwrap();
        let op = Operator::start(
            "vitals",
            &[
                ("PATH", m.dir.join("bin").into()),
                ("HOME", m.dir.join("home").into()),
                ("LEGIONCTL_SYSROOT", m.dir.join("root").into()),
                ("DBUS_SYSTEM_BUS_ADDRESS", address.clone().into()),
                ("DBUS_SESSION_BUS_ADDRESS", address.into()),
            ],
        );
        let vitals = || -> Vec<String> {
            shown
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|(kind, _)| kind == "vitals")
                .map(|(_, v)| v.clone())
                .collect()
        };
        std::thread::sleep(std::time::Duration::from_secs(3));
        assert!(vitals().is_empty(), "nothing while it is off");

        let on = op.ok(&["overlay", "on"]);
        assert!(on.contains("CPU  71 °C"), "{on}");
        wait_for("the overlay", || !vitals().is_empty());
        let first = vitals()[0].clone();
        assert!(
            first.starts_with("CPU  71 °C  ")
                && first.contains("\nGPU  asleep (D3cold)\n")
                && first.contains("\nFans 5100 / 5100 RPM\n")
                && first.ends_with("max-power on ac"),
            "{first}"
        );
        m.set(gpu, "active");
        wait_for("the GPU awake", || {
            vitals()
                .last()
                .is_some_and(|v| v.contains("\nGPU  51 °C  42 W\n"))
        });

        op.ok(&["overlay", "off"]);
        wait_for("the overlay hidden", || {
            vitals().last().is_some_and(String::is_empty)
        });
        let count = vitals().len();
        std::thread::sleep(std::time::Duration::from_secs(3));
        assert_eq!(vitals().len(), count, "one hide, then nothing");
    }
}
