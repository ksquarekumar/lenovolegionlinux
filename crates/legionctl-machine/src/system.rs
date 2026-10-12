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
pub fn unit_state(query: &str, unit: &str) -> String {
    match output("systemctl", &[query, unit]) {
        Some((_, text)) if !text.is_empty() => text,
        _ => "unknown".into(),
    }
}

pub fn package_installed(package: &str) -> bool {
    output("pacman", &["-Q", package]).is_some_and(|(ok, _)| ok)
}

/// Current NVIDIA board power limit in W, from `nvidia-smi -q -d POWER` (`--query-gpu=power.limit` prints `[N/A]`
/// with this driver).
pub fn nvidia_power_limit() -> Option<String> {
    let (ok, text) = output("nvidia-smi", &["-q", "-d", "POWER"])?;
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with("Current Power Limit"))?;
    let value = line.split(':').nth(1)?.trim().trim_end_matches('W').trim();
    (ok && value.parse::<f64>().is_ok()).then(|| value.to_string())
}
