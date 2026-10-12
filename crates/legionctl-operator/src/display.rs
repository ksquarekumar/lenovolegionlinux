//! The built-in panel through Mutter's `DisplayConfig` API: its current refresh rate, and the Fn+R toggle between the
//! highest rate and ~60 Hz at the current resolution and VRR state. Changes use the TEMPORARY method, which applies
//! without GNOME's "Keep these display settings?" dialog; the saved layout returns at the next login or hotplug.

use std::collections::HashMap;

use anyhow::{Context, Result};
use zbus::zvariant::{OwnedValue, Value};

/// (id, width, height, refresh rate, preferred scale, supported scales, properties)
pub type Mode = (
    String,
    i32,
    i32,
    f64,
    f64,
    Vec<f64>,
    HashMap<String, OwnedValue>,
);
/// (connector, vendor, product, serial)
pub type MonitorSpec = (String, String, String, String);
pub type Monitor = (MonitorSpec, Vec<Mode>, HashMap<String, OwnedValue>);
/// (x, y, scale, transform, primary, monitors, properties)
pub type LogicalMonitor = (
    i32,
    i32,
    f64,
    u32,
    bool,
    Vec<MonitorSpec>,
    HashMap<String, OwnedValue>,
);
/// (serial, monitors, logical monitors, properties)
pub type State = (
    u32,
    Vec<Monitor>,
    Vec<LogicalMonitor>,
    HashMap<String, OwnedValue>,
);
/// (x, y, scale, transform, primary, [(connector, mode id, properties)])
pub type LogicalMonitorConfig<'a> = (
    i32,
    i32,
    f64,
    u32,
    bool,
    Vec<(String, String, HashMap<String, Value<'a>>)>,
);

const TEMPORARY: u32 = 1;
const LOW_HZ: f64 = 60.0;

#[zbus::proxy(
    default_service = "org.gnome.Mutter.DisplayConfig",
    default_path = "/org/gnome/Mutter/DisplayConfig",
    interface = "org.gnome.Mutter.DisplayConfig"
)]
pub trait DisplayConfig {
    fn get_current_state(&self) -> zbus::Result<State>;

    fn apply_monitors_config(
        &self,
        serial: u32,
        method: u32,
        logical_monitors: Vec<LogicalMonitorConfig<'_>>,
        properties: HashMap<String, Value<'_>>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    fn monitors_changed(&self) -> zbus::Result<()>;
}

fn prop_bool(mode: &Mode, key: &str) -> bool {
    mode.6
        .get(key)
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false)
}

fn refresh_kind(mode: &Mode) -> String {
    mode.6
        .get("refresh-rate-mode")
        .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
        .unwrap_or_else(|| "fixed".into())
}

pub fn current_mode(modes: &[Mode]) -> Option<&Mode> {
    modes.iter().find(|m| prop_bool(m, "is-current"))
}

/// The highest rate at the current resolution and refresh kind (fixed or VRR); from there, the mode closest to 60 Hz.
pub fn pick_target<'a>(modes: &'a [Mode], current: &Mode) -> &'a Mode {
    let same: Vec<&Mode> = modes
        .iter()
        .filter(|m| {
            (m.1, m.2) == (current.1, current.2) && refresh_kind(m) == refresh_kind(current)
        })
        .collect();
    let highest = same
        .iter()
        .copied()
        .max_by(|a, b| a.3.total_cmp(&b.3))
        .unwrap_or(&modes[0]);
    if (current.3 - highest.3).abs() > 0.5 {
        return highest;
    }
    same.into_iter()
        .min_by(|a, b| (a.3 - LOW_HZ).abs().total_cmp(&(b.3 - LOW_HZ).abs()))
        .unwrap_or(highest)
}

/// The built-in (eDP) panel's connector and modes, when it is active (part of a logical monitor).
fn built_in(monitors: &[Monitor], logical: &[LogicalMonitor]) -> Option<(String, Vec<Mode>)> {
    let active: Vec<&String> = logical
        .iter()
        .flat_map(|l| l.5.iter().map(|s| &s.0))
        .collect();
    monitors
        .iter()
        .find(|m| m.0.0.starts_with("eDP") && active.contains(&&m.0.0))
        .map(|m| (m.0.0.clone(), m.1.clone()))
}

/// Current refresh rate of the active built-in panel in whole Hz, as the log and the OSD show it.
pub async fn built_in_rate(mutter: &DisplayConfigProxy<'_>) -> Option<String> {
    let (_, monitors, logical, _) = mutter.get_current_state().await.ok()?;
    let (_, modes) = built_in(&monitors, &logical)?;
    current_mode(&modes).map(|m| format!("{:.0}", m.3))
}

/// Fn+R: switch the built-in panel to the other rate, keeping every other monitor as it is.
pub async fn toggle_refresh(mutter: &DisplayConfigProxy<'_>) -> Result<String> {
    let (serial, monitors, logical, _) = mutter.get_current_state().await?;
    let Some((panel, modes)) = built_in(&monitors, &logical) else {
        return Ok("built-in panel is not active; nothing to toggle".into());
    };
    let current = current_mode(&modes).context("built-in panel has no current mode")?;
    let target = pick_target(&modes, current);
    let current_of = |connector: &str| -> Option<String> {
        let (_, modes, _) = monitors.iter().find(|m| m.0.0 == connector)?;
        current_mode(modes).map(|m| m.0.clone())
    };
    let config: Vec<LogicalMonitorConfig> = logical
        .iter()
        .map(|l| {
            let members =
                l.5.iter()
                    .map(|spec| {
                        let mode = if spec.0 == panel {
                            target.0.clone()
                        } else {
                            current_of(&spec.0).unwrap_or_default()
                        };
                        (spec.0.clone(), mode, HashMap::new())
                    })
                    .collect();
            (l.0, l.1, l.2, l.3, l.4, members)
        })
        .collect();
    mutter
        .apply_monitors_config(serial, TEMPORARY, config, HashMap::new())
        .await?;
    Ok(format!(
        "{panel}: {:.0} Hz -> {:.0} Hz",
        current.3, target.3
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(w: i32, h: i32, hz: f64, current: bool, vrr: bool) -> Mode {
        let props = HashMap::from([
            ("is-current".to_string(), OwnedValue::from(current)),
            (
                "refresh-rate-mode".to_string(),
                OwnedValue::try_from(Value::from(if vrr { "variable" } else { "fixed" })).unwrap(),
            ),
        ]);
        (
            format!("{w}x{h}@{hz}{}", if vrr { "+vrr" } else { "" }),
            w,
            h,
            hz,
            1.0,
            vec![1.0],
            props,
        )
    }

    /// The panel's modes as Mutter lists them on this model, plus a lower resolution.
    fn panel() -> Vec<Mode> {
        vec![
            mode(2560, 1600, 240.003, false, false),
            mode(2560, 1600, 240.003, false, true),
            mode(2560, 1600, 60.002, false, false),
            mode(2560, 1600, 59.987, false, false),
            mode(1920, 1200, 240.0, false, false),
        ]
    }

    #[test]
    fn fn_r_toggles_rate_at_the_same_resolution_and_refresh_kind() {
        let p = panel();
        assert_eq!(
            pick_target(&p, &p[0]).0,
            "2560x1600@60.002",
            "high goes to the mode closest to 60 Hz"
        );
        assert_eq!(
            pick_target(&p, &p[2]).0,
            "2560x1600@240.003",
            "low goes to the highest fixed mode, not VRR"
        );
        assert_eq!(
            (pick_target(&p, &p[3]).1, pick_target(&p, &p[3]).2),
            (2560, 1600),
            "same resolution"
        );

        let mut vrr = panel();
        vrr.push(mode(2560, 1600, 60.002, false, true));
        let low = pick_target(&vrr, &vrr[1]);
        assert_eq!(low.0, "2560x1600@60.002+vrr", "VRR stays VRR going down");
        assert_eq!(
            pick_target(&vrr, low).0,
            "2560x1600@240.003+vrr",
            "and going up"
        );
    }
}
