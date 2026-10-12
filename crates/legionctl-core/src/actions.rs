//! Actions that change the machine. Each is a method call to the machine daemon (`org.legionctl.System1`), which asks
//! polkit, so a human approves it in the desktop's dialog; agents can request actions but never perform them silently.

use std::process::Command;

use anyhow::{Context, Result, ensure};

/// The exact command an approved action runs, shown to the human before they approve.
///
/// # Errors
/// `action` is not on the allow-list, or `value` is not valid for it.
pub fn approved_command(action: &str, value: &str) -> Result<Vec<String>> {
    let method = match action {
        "apply-power-policy" => {
            ensure!(
                matches!(value, "auto" | "ac" | "battery"),
                "power policy must be auto, ac or battery"
            );
            "Apply"
        }
        "set-desktop-gpu" => {
            ensure!(
                matches!(value, "nvidia" | "intel" | "auto"),
                "desktop GPU must be nvidia, intel or auto"
            );
            "SetDesktopGpu"
        }
        _ => anyhow::bail!("unknown action {action}; known: apply-power-policy, set-desktop-gpu"),
    };
    Ok([
        "busctl",
        "call",
        "org.legionctl.System1",
        "/org/legionctl/System1",
        "org.legionctl.System1",
        method,
        "s",
        value,
    ]
    .map(String::from)
    .to_vec())
}

/// Runs an action after polkit approves it. Returns what the daemon changed, one line each; a refusal is an error.
///
/// # Errors
/// The action is not allowed, `busctl` does not run, or polkit or the daemon refuses.
pub fn run_approved(action: &str, value: &str) -> Result<String> {
    let argv = approved_command(action, value)?;
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .context("running busctl")?;
    ensure!(
        out.status.success(),
        "not applied: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    // busctl prints the string array as: as N "line" "line" ...
    let reply = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = reply.split('"').skip(1).step_by(2).collect();
    Ok(if lines.is_empty() {
        "no changes\n".into()
    } else {
        format!("{}\n", lines.join("\n"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_actions_and_values_reach_the_daemon() {
        assert_eq!(
            approved_command("set-desktop-gpu", "nvidia").unwrap()[5..],
            ["SetDesktopGpu", "s", "nvidia"]
        );
        assert!(approved_command("apply-power-policy", "auto; rm -rf /").is_err());
        assert!(approved_command("set-desktop-gpu", "--force").is_err());
        assert!(approved_command("reboot", "now").is_err());
    }
}
