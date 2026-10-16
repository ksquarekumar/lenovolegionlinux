//! Questions answered by system tools: systemd unit state, installed packages, the NVIDIA power limit. A tool that
//! does not run or fails is an error with its cause; only an answer the tool gives (a unit that is not found, a
//! package that is not installed) is a state.

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

/// A GNOME Shell extension's `State:` (ACTIVE, ERROR, ...), `not installed`, or `None` without `gnome-extensions`.
///
/// # Errors
/// `gnome-extensions` fails for another reason (no GNOME Shell on the session bus).
pub fn extension_state(uuid: &str) -> Result<Option<String>> {
    let Some(out) = output("gnome-extensions", &["info", uuid])? else {
        return Ok(None);
    };
    if !out.ok {
        if format!("{}{}", out.stdout, out.stderr).contains("doesn't exist") {
            return Ok(Some("not installed".into()));
        }
        bail!(
            "gnome-extensions info {uuid} failed (does GNOME Shell run in this session?): {}",
            out.stderr
        );
    }
    out.stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("State:"))
        .map(|s| Some(s.trim().to_string()))
        .with_context(|| format!("gnome-extensions info {uuid} gave no State: {}", out.stdout))
}

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
