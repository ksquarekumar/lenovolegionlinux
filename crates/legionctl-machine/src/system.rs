//! Questions answered by system tools: systemd unit state, installed packages, the NVIDIA power limit. A missing
//! tool gives `unknown`/`None`, never an error.

use std::process::Command;

fn output(program: &str, args: &[&str]) -> Option<(bool, String)> {
    let out = Command::new(program).args(args).output().ok()?;
    Some((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

/// `systemctl is-active|is-enabled`; `unknown` when systemctl is missing or prints nothing.
#[must_use]
pub fn unit_state(query: &str, unit: &str) -> String {
    match output("systemctl", &[query, unit]) {
        Some((_, text)) if !text.is_empty() => text,
        _ => "unknown".into(),
    }
}

/// Failed units of the system manager, or of the user's with `user`; empty when systemctl is missing.
#[must_use]
pub fn failed_units(user: bool) -> Vec<String> {
    let args = ["--user", "--failed", "--plain", "--no-legend"];
    output("systemctl", &args[usize::from(!user)..])
        .map(|(_, text)| {
            text.lines()
                .filter_map(|l| l.split_whitespace().next().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// A GNOME Shell extension's `State:` (ACTIVE, ERROR, ...), `not installed`, or `None` without `gnome-extensions`.
#[must_use]
pub fn extension_state(uuid: &str) -> Option<String> {
    let (ok, text) = output("gnome-extensions", &["info", uuid])?;
    if !ok {
        return Some("not installed".into());
    }
    text.lines()
        .find_map(|l| l.trim().strip_prefix("State:"))
        .map(|s| s.trim().to_string())
}

#[must_use]
pub fn package_installed(package: &str) -> bool {
    output("pacman", &["-Q", package]).is_some_and(|(ok, _)| ok)
}

/// Current NVIDIA board power limit in W, from `nvidia-smi -q -d POWER` (`--query-gpu=power.limit` prints `[N/A]`
/// with this driver).
#[must_use]
pub fn nvidia_power_limit() -> Option<String> {
    let (ok, text) = output("nvidia-smi", &["-q", "-d", "POWER"])?;
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with("Current Power Limit"))?;
    let value = line.split(':').nth(1)?.trim().trim_end_matches('W').trim();
    (ok && value.parse::<f64>().is_ok()).then(|| value.to_string())
}
