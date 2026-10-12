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
        if entry.file_type().unwrap().is_dir() {
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
