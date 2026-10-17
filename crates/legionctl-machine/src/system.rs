//! Questions answered by system tools: systemd unit state, installed packages, the NVIDIA power limit. A tool that
//! does not run or fails is an error with its cause; only an answer the tool gives (a unit that is not found, a
//! package that is not installed) is a state.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

struct Out {
    ok: bool,
    stdout: String,
    stderr: String,
}

/// `program args`; `None` when the program is not installed.
fn output(program: &str, args: &[&str]) -> Result<Option<Out>> {
    let out = match Command::new(program).args(args).output() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        out => out.with_context(|| format!("running {program}"))?,
    };
    Ok(Some(Out {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    }))
}

/// `systemctl is-active|is-enabled`: `active`, `inactive`, `not-found`, ...
///
/// # Errors
/// systemctl is not installed, does not run, or gives no state (no system bus).
pub fn unit_state(query: &str, unit: &str) -> Result<String> {
    let out = output("systemctl", &[query, unit])?.context("systemctl is not installed")?;
    if out.stdout.is_empty() {
        bail!("systemctl {query} {unit} gave no state: {}", out.stderr);
    }
    Ok(out.stdout)
}

/// Failed units of the system manager, or of the user's with `user`.
///
/// # Errors
/// systemctl is not installed, does not run, or fails.
pub fn failed_units(user: bool) -> Result<Vec<String>> {
    let args = ["--user", "--failed", "--plain", "--no-legend"];
    let out =
        output("systemctl", &args[usize::from(!user)..])?.context("systemctl is not installed")?;
    anyhow::ensure!(out.ok, "systemctl --failed failed: {}", out.stderr);
    Ok(out
        .stdout
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(String::from))
        .collect())
}

/// The package's OSD GNOME Shell extension.
pub const OSD_EXTENSION: &str = "legionctl-osd@legionctl";

/// The OSD extension's state as `setup check` and `doctor` say it: `current` (it runs, or runs from the next login),
/// `not installed`, `not enabled` or [`OSD_BROKEN`]; `None` without `gnome_extensions`.
///
/// # Errors
/// `gnome-extensions` fails for another reason (no GNOME Shell on the session bus).
pub fn osd_status(gnome_extensions: &Path) -> Result<Option<&'static str>> {
    let out = match Command::new(gnome_extensions)
        .args(["info", OSD_EXTENSION])
        .output()
    {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        out => out.context("running gnome-extensions")?,
    };
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    if !out.status.success() {
        if format!("{stdout}{stderr}").contains("doesn't exist") {
            return Ok(Some("not installed"));
        }
        bail!(
            "GNOME Shell does not answer in this session: {}",
            stderr.trim()
        );
    }
    let field = |name: &str| {
        stdout
            .lines()
            .find_map(|l| l.trim().strip_prefix(name))
            .map(str::trim)
    };
    Ok(Some(match (field("State:"), field("Enabled:")) {
        // ENABLED is the name of ACTIVE before GNOME 45
        (Some("ERROR" | "OUT OF DATE"), _) => OSD_BROKEN,
        (Some("ACTIVE" | "ENABLED"), _) | (_, Some("Yes")) => "current",
        _ => "not enabled",
    }))
}

/// The OSD extension failed in this GNOME Shell: legionctl's OSD is broken here, and no setup step mends it.
pub const OSD_BROKEN: &str = "broken in this GNOME Shell: Fn+Q, Fn+R and Fn lock show no OSD";

/// Whether pacman has the package `package` installed.
///
/// # Errors
/// pacman is not installed, does not run, or fails for another reason than a package it does not have.
pub fn package_installed(package: &str) -> Result<bool> {
    let out = output("pacman", &["-Q", package])?.context("pacman is not installed")?;
    if !out.ok && !out.stderr.contains("was not found") {
        bail!("pacman -Q {package} failed: {}", out.stderr);
    }
    Ok(out.ok)
}

/// Current NVIDIA board power limit in W, from `nvidia-smi -q -d POWER` (`--query-gpu=power.limit` prints `[N/A]`
/// with this driver).
#[must_use]
pub fn nvidia_power_limit() -> Option<String> {
    // shortcut: a failed nvidia-smi reads as no limit; upgrade when the snapshot carries a reason per value
    let Out { ok, stdout, .. } = output("nvidia-smi", &["-q", "-d", "POWER"]).ok()??;
    let line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("Current Power Limit"))?;
    let value = line.split(':').nth(1)?.trim().trim_end_matches('W').trim();
    (ok && value.parse::<f64>().is_ok()).then(|| value.to_string())
}
