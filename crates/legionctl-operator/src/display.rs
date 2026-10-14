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

/// One active monitor as `display status` shows it.
pub struct Shown {
    pub connector: String,
    pub name: String,
    pub builtin: bool,
    pub width: i32,
    pub height: i32,
    pub refresh: f64,
    /// The highest rate at the current resolution and refresh kind.
    pub max_refresh: f64,
    pub scale: f64,
    /// Pixels per inch, from the EDID's physical size.
    pub ppi: Option<f64>,
    /// A variable-rate mode exists at this resolution, and whether it is in use.
    pub vrr: (bool, bool),
    /// HDR (a color mode beyond the default) is offered, and whether it is in use.
    pub hdr: (bool, bool),
}

/// The physical size (cm) from the EDID of `connector` (bytes 21 and 22), when the monitor reports one.
fn edid_size(root: &legionctl_machine::sysroot::SysRoot, connector: &str) -> Option<(f64, f64)> {
    let card = root
        .children("/sys/class/drm", "card")
        .into_iter()
        .find(|c| c.to_string_lossy().ends_with(&format!("-{connector}")))?;
    let edid = std::fs::read(root.path(card.join("edid"))).ok()?;
    let (w, h) = (*edid.get(21)?, *edid.get(22)?);
    (w > 0 && h > 0).then(|| (f64::from(w), f64::from(h)))
}

/// The active monitors of Mutter's `state`.
#[must_use]
pub fn shown(root: &legionctl_machine::sysroot::SysRoot, state: &State) -> Vec<Shown> {
    let (_, monitors, logical, _) = state;
    let mut out = Vec::new();
    for l in logical {
        for spec in &l.5 {
            let Some(monitor) = monitors.iter().find(|m| m.0.0 == spec.0) else {
                continue;
            };
            let Some(current) = current_mode(&monitor.1) else {
                continue;
            };
            let same_size: Vec<&Mode> = monitor
                .1
                .iter()
                .filter(|m| (m.1, m.2) == (current.1, current.2))
                .collect();
            let max_refresh = same_size
                .iter()
                .filter(|m| refresh_kind(m) == refresh_kind(current))
                .map(|m| m.3)
                .fold(current.3, f64::max);
            let prop = |key: &str| monitor.2.get(key).and_then(|v| v.try_clone().ok());
            let name = prop("display-name")
                .and_then(|v| String::try_from(v).ok())
                .unwrap_or_default();
            let builtin = prop("is-builtin")
                .and_then(|v| bool::try_from(v).ok())
                .unwrap_or(false);
            let color = |key: &str| prop(key).and_then(|v| u32::try_from(v).ok());
            let colors = prop("supported-color-modes")
                .and_then(|v| Vec::<u32>::try_from(v).ok())
                .unwrap_or_default();
            let ppi = edid_size(root, &spec.0).map(|(w, h)| {
                let pixels = f64::from(current.1).hypot(f64::from(current.2));
                pixels / (w.hypot(h) / 2.54)
            });
            out.push(Shown {
                connector: spec.0.clone(),
                name,
                builtin,
                width: current.1,
                height: current.2,
                refresh: current.3,
                max_refresh,
                scale: l.2,
                ppi,
                vrr: (
                    same_size.iter().any(|m| refresh_kind(m) == "variable"),
                    refresh_kind(current) == "variable",
                ),
                hdr: (
                    colors.iter().any(|c| *c != 0),
                    color("color-mode").is_some_and(|c| c != 0),
                ),
            });
        }
    }
    out
}

/// What makes the picture worse than it can be: an external monitor below its highest rate, apps through `XWayland`
/// at a fractional scale (`xwayland_apps`: the apps without a Wayland flag), and grayscale
/// antialiasing where subpixel is sharper (integer scales at normal density; grayscale helps only at fractional or
/// high-density scales: user report 2026-10-14).
#[must_use]
pub fn findings(monitors: &[Shown], antialiasing: &str, xwayland_apps: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = monitors
        .iter()
        .filter(|m| !m.builtin && m.max_refresh - m.refresh > 0.5)
        .map(|m| {
            format!(
                "{} ({}) runs at {:.0} Hz, and {:.0} Hz is available at {}x{} (Settings, Displays, Refresh Rate)",
                m.name, m.connector, m.refresh, m.max_refresh, m.width, m.height
            )
        })
        .collect();
    let sharp_subpixel = monitors
        .iter()
        .all(|m| m.scale.fract() == 0.0 && m.ppi.is_none_or(|p| p < 160.0));
    // XWayland is blurry only at a fractional scale; at integer scales Chromium on Wayland is the worse one
    if monitors.iter().any(|m| m.scale.fract() != 0.0) {
        out.extend(xwayland_apps);
    }
    if antialiasing == "grayscale" && !monitors.is_empty() && sharp_subpixel {
        out.push(
            "fonts use grayscale antialiasing, and every monitor has an integer scale at normal density, where \
             subpixel is sharper (gsettings set org.gnome.desktop.interface font-antialiasing rgba)"
                .into(),
        );
    }
    out
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
