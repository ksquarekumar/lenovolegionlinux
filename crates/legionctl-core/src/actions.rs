//! Actions that change the machine. Each runs `legionctl` through `pkexec`, so a human approves it in the
//! desktop's polkit dialog every time; agents can request actions but never perform them silently.

use std::process::Command;

use anyhow::{Context, Result, ensure};

const LEGIONCTL: &str = "/usr/bin/legionctl";

/// The exact command an approved action would run, shown to the human before they approve.
pub fn approved_command(action: &str, value: &str) -> Result<Vec<String>> {
    let args: Vec<&str> = match action {
        "apply-power-policy" => {
            ensure!(
                matches!(value, "auto" | "ac" | "battery"),
                "power policy must be auto, ac or battery"
            );
            vec!["apply", value]
        }
        "set-desktop-gpu" => {
            ensure!(
                matches!(value, "nvidia" | "intel" | "auto"),
                "desktop GPU must be nvidia, intel or auto"
            );
            vec!["gpu", "primary", value]
        }
        _ => anyhow::bail!("unknown action {action}; known: apply-power-policy, set-desktop-gpu"),
    };
    Ok(std::iter::once("pkexec")
        .chain(std::iter::once(LEGIONCTL))
        .chain(args)
        .map(String::from)
        .collect())
}

/// Runs an action after the polkit dialog is approved. Returns legionctl's output; a refusal is an error.
pub fn run_approved(action: &str, value: &str) -> Result<String> {
    let argv = approved_command(action, value)?;
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .context("running pkexec")?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // pkexec exits 126 when the human dismisses the dialog and 127 when authorization fails
    ensure!(
        out.status.success(),
        "not applied ({}): {}",
        out.status,
        text.trim()
    );
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_actions_and_values_pass() {
        assert_eq!(
            approved_command("set-desktop-gpu", "nvidia").unwrap(),
            ["pkexec", LEGIONCTL, "gpu", "primary", "nvidia"]
        );
        assert_eq!(
            approved_command("apply-power-policy", "auto").unwrap(),
            ["pkexec", LEGIONCTL, "apply", "auto"]
        );
        assert!(approved_command("apply-power-policy", "auto; rm -rf /").is_err());
        assert!(approved_command("set-desktop-gpu", "--force").is_err());
        assert!(approved_command("reboot", "now").is_err());
    }
}
