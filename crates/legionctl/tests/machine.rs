//! End-to-end: the power policy commands against a recorded sysroot of this machine (2026-10-12, on AC with the AC
//! policy applied), with logging stand-ins for powerprofilesctl, cpupower, systemctl and nvidia-smi.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FIXTURE: &str = "../../tests/fixtures/sysroot-82wq-2026-10-12";
const PROFILE: &str = "../../profiles/lenovo-82wq-legion-pro-7-16irx8h.json";

struct Machine {
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
    fn new(tag: &str) -> Machine {
        let dir =
            std::env::temp_dir().join(format!("legionctl-machine-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE),
            &dir.join("root"),
        );
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = dir.join("calls.log");
        let ppd = dir.join("ppd");
        std::fs::write(&ppd, "performance\n").unwrap();
        for name in ["cpupower", "systemctl", "nvidia-smi"] {
            Self::script(
                &bin,
                name,
                &format!("echo \"{name} $*\" >> {}", log.display()),
            );
        }
        Self::script(
            &bin,
            "powerprofilesctl",
            // like the real daemon: intel_pstate refuses EPP changes while the governor is performance
            &format!(
                "if [ \"$1\" = get ]; then read -r p < {ppd}; echo \"$p\"; exit 0; fi\n\
                 if [ \"$2\" != performance ] && {{ read -r g < {gov}; [ \"$g\" = performance ]; }}; then\n\
                 echo \"Failed to activate CPU driver intel_pstate: Device or resource busy\" >&2; exit 1; fi\n\
                 echo \"powerprofilesctl $*\" >> {log}; echo \"$2\" > {ppd}",
                ppd = ppd.display(),
                log = log.display(),
                gov = dir
                    .join("root/sys/devices/system/cpu/cpufreq/policy0/scaling_governor")
                    .display()
            ),
        );
        Machine { dir }
    }

    fn script(bin: &Path, name: &str, body: &str) {
        let path = bin.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    }

    fn file(&self, machine_path: &str) -> String {
        std::fs::read_to_string(self.dir.join("root").join(&machine_path[1..]))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "<missing>".into())
    }

    fn set(&self, machine_path: &str, value: &str) {
        std::fs::write(
            self.dir.join("root").join(&machine_path[1..]),
            format!("{value}\n"),
        )
        .unwrap();
    }

    fn legion(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_legion"))
            .args(args)
            .args([
                "--profile",
                &Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(PROFILE)
                    .display()
                    .to_string(),
            ])
            .env_clear()
            .env("PATH", self.dir.join("bin"))
            .env("LEGIONCTL_SYSROOT", self.dir.join("root"))
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.legion(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap() + &String::from_utf8_lossy(&out.stderr)
    }

    /// Calls made to the stand-in commands since the last check.
    fn calls(&self) -> Vec<String> {
        let log = self.dir.join("calls.log");
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        let _ = std::fs::remove_file(&log);
        text.lines().map(String::from).collect()
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
const OVERRIDE: &str = "/run/legionctl/firmware-override";
const WRITTEN: &str = "/run/legionctl/firmware-written";

#[test]
fn a_machine_already_in_ac_policy_gets_no_writes() {
    let m = Machine::new("steady");
    let out = m.ok(&["apply", "auto", "--dry-run"]);
    assert!(!out.contains("would write"), "{out}");
    assert!(out.ends_with("applied ac policy\n"), "{out}");
}

#[test]
fn unplug_fn_q_resume_and_replug_follow_the_policy_and_the_users_choice() {
    let m = Machine::new("cycle");

    // unplug: battery policy, firmware balanced, recorded as our own write
    m.set("/sys/class/power_supply/ADP0/online", "0");
    let out = m.ok(&["apply", "auto", "--quiet"]);
    assert!(!out.contains("warning"), "unplug applies cleanly: {out}");
    assert_eq!(m.file(PROFILE_PATH), "balanced");
    assert_eq!(m.file(WRITTEN), "balanced");
    assert_eq!(m.governors(), ["powersave"]);
    assert_eq!(
        m.file("/sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference"),
        "balance_power"
    );
    let calls = m.calls();
    assert!(
        calls.contains(&"powerprofilesctl set balanced".into()),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.contains("nvidia")),
        "no NVIDIA services on battery: {calls:?}"
    );

    // our own write produces the same uevent as Fn+Q; following it must change nothing
    assert!(m.ok(&["follow"]).contains("nothing to follow"));

    // Fn+Q: the firmware switches to performance; that is the user's choice
    m.set(PROFILE_PATH, "performance");
    m.ok(&["follow", "--quiet"]);
    assert_eq!(m.file(OVERRIDE), "performance");
    assert_eq!(m.governors(), ["performance"]);
    assert_eq!(m.calls(), ["powerprofilesctl set performance"]);

    // reassert and resume keep the choice; neither writes the firmware mode back
    m.ok(&["reassert", "battery", "--quiet"]);
    m.ok(&["apply", "auto", "--keep-override", "--quiet"]);
    assert_eq!(m.file(PROFILE_PATH), "performance");
    assert_eq!(m.file(OVERRIDE), "performance");

    // plug in: a power-source change starts from the profile again
    m.set("/sys/class/power_supply/ADP0/online", "1");
    let out = m.ok(&["apply", "auto"]);
    assert!(
        out.contains("clearing firmware override 'performance'"),
        "{out}"
    );
    assert_eq!(m.file(OVERRIDE), "<missing>");
    assert_eq!(m.file(PROFILE_PATH), "max-power");
    assert_eq!(m.file(WRITTEN), "max-power");
    assert!(m.calls().contains(&"nvidia-smi -i 0 -pm 1".into()));
}

#[test]
fn refuses_to_apply_on_a_machine_that_is_not_this_model() {
    let m = Machine::new("dmi");
    m.set("/sys/class/dmi/id/product_name", "82WK");
    m.set("/sys/class/power_supply/ADP0/online", "0");
    let out = m.legion(&["apply", "auto"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("product_name: expected '82WQ', got '82WK'")
    );
    assert_eq!(m.file(PROFILE_PATH), "max-power", "nothing written");
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
            "- an external display runs on NVIDIA but the desktop may be drawn on Intel; try `legionctl gpu primary nvidia`",
        ]
    );
}

#[test]
fn cleanup_disables_running_conflicts_and_only_warns_about_packages_unless_asked() {
    let m = Machine::new("cleanup");
    let bin = m.dir.join("bin");
    Machine::script(
        &bin,
        "systemctl",
        &format!(
            "echo \"systemctl $*\" >> {log}; \
             if [ \"$1\" = is-active ] && [ \"$2\" = legiond.service ]; then echo active; \
             elif [ \"$1\" = is-active ]; then echo inactive; elif [ \"$1\" = is-enabled ]; then echo disabled; fi",
            log = m.dir.join("calls.log").display()
        ),
    );
    Machine::script(
        &bin,
        "pacman",
        &format!(
            "echo \"pacman $*\" >> {}; [ \"$2\" = lenovolegionlinux-git ]",
            m.dir.join("calls.log").display()
        ),
    );

    let out = m.ok(&["cleanup"]);
    assert!(out.contains("conflicting package is installed but was not removed without --remove-packages: lenovolegionlinux-git"), "{out}");
    let calls = m.calls();
    assert!(
        calls.contains(&"systemctl disable --now legiond.service".into()),
        "{calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("systemctl disable --now powersaver")),
        "quiet units stay: {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("pacman -Rns")),
        "{calls:?}"
    );

    m.ok(&["cleanup", "--remove-packages", "--quiet"]);
    assert!(
        m.calls()
            .contains(&"pacman -Rns lenovolegionlinux-git".into())
    );
}

/// The daemon's decisions per event, planned (not executed) against the recorded machine.
mod daemon_events {
    use super::*;
    use legionctl_machine::policy::{self, Machine as Planner, Step};
    use legionctl_machine::react::{Event, react};
    use legionctl_machine::sysroot::SysRoot;

    fn plan(m: &Machine, event: Event) -> Vec<String> {
        let (profile, _) = legionctl_machine::profile::load(Some(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join(PROFILE),
        ))
        .unwrap();
        let root = SysRoot::new(m.dir.join("root"));
        let planner = Planner {
            root: &root,
            profile: &profile,
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
        m.set(WRITTEN, "max-power");
        assert_eq!(
            plan(&m, Event::PlatformProfile),
            ["platform_profile 'max-power' is already the active policy; nothing to follow"]
        );
        // Fn+Q to performance
        m.set(PROFILE_PATH, "performance");
        m.set(OVERRIDE, "performance");
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
