//! Fan control of this model (docs/design/02-platform.md, Fan control): the fan curve, fan max mode and full speed
//! through the `legionctl-wmi` module's hwmon device (docs/design/08-kernel.md, sysfs ABI), and the EC fan lock (a raw
//! EC write) only when the profile marks it verified. Plans are steps like the power policy's: printed in a dry run,
//! carried out by `legionctl-system`.

use std::os::unix::fs::FileExt;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::firmware::{legionctl_wmi, read_number};
use crate::ledger::FanKey;
use crate::policy::{Machine, Step};
use crate::profile::FanControl;
use crate::sysroot::SysRoot;

/// The points of the table (FSS0-FSS9): one level each, from the lowest temperature band to the highest.
pub const POINTS: usize = 10;
/// The lowest level the firmware accepts at each point.
pub const MINIMUM: [u8; POINTS] = [1, 1, 1, 1, 1, 1, 1, 1, 3, 5];
/// RPM of each level 1-10 (WQA3 `LENOVO_FAN_TABLE_DATA`, measured 2026-10-12, the same in every mode).
pub const RPM: [[u32; POINTS]; 2] = [
    [1700, 1900, 2100, 2200, 2600, 2800, 3300, 3500, 4400, 5100],
    [1700, 1800, 2100, 2300, 2600, 2800, 3300, 3600, 4400, 5100],
];
/// The `platform_profile` of firmware mode 255, the only mode that keeps a written table.
pub const CUSTOM: &str = "custom";

/// The module's fan max mode file: 1 on, 0 off, in all modes.
pub const MAX_MODE: &str = "fan_max_mode";
/// The module's fan mode file: 2 the table, 0 full speed (custom mode only).
pub const ENABLE: &str = "pwm1_enable";
/// EC RAM byte of the fan controller lock (`LenovoLegionLinux` `EXT_LOCKFANCONTROLLER`): 0x08 locks, 0x00 releases.
pub const EC_FAN_LOCK: u16 = 0xC4AB;
const EC_LOCKED: u8 = 0x08;
/// The EC's index and data ports (`LenovoLegionLinux` `ecram_portio_write`).
const EC_INDEX: u64 = 0x4E;
const EC_DATA: u64 = 0x4F;

/// The fan controls as `legionctl-system` knows them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanState {
    /// `platform_profile` now; the curve applies in `custom` only.
    pub mode: Option<String>,
    /// The table the firmware has now; `None` when it cannot be read.
    pub curve: Option<Vec<u8>>,
    /// Why the table cannot be read.
    pub curve_error: Option<String>,
    /// The user's curve, applied at every entry to custom mode and after resume.
    pub chosen: Option<Vec<u8>>,
    /// The curve the profile gives when the user has none (`fan curve reset`).
    pub default_curve: Vec<u8>,
    /// Fan max mode as the module reports it (the state that it set; the firmware has no read).
    pub max: bool,
    /// Full speed (dust cleaning) is on.
    pub full_speed: bool,
    /// RPM of fan 1 and fan 2 at each level 1-10.
    pub fan1_rpm: Vec<u32>,
    pub fan2_rpm: Vec<u32>,
    /// The profile allows the EC fan lock.
    pub ec_fan_lock_verified: bool,
}

/// The levels as the ledger keeps them and messages show them.
#[must_use]
pub fn text(levels: &[u8]) -> String {
    levels
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Check a curve before anything is written.
///
/// # Errors
/// Not 10 levels, a level outside 1-10 or below the point's minimum, or a level lower than the point before.
pub fn validate(levels: &[u8]) -> Result<[u8; POINTS]> {
    let curve: [u8; POINTS] = levels.try_into().map_err(|_| {
        anyhow::anyhow!(
            "a fan curve has {POINTS} levels, one for each point; you gave {}",
            levels.len()
        )
    })?;
    for (i, (level, minimum)) in curve.iter().zip(MINIMUM).enumerate() {
        let point = i + 1;
        ensure!(
            (1..=10).contains(level),
            "point {point} is {level}: a level is 1 to 10 (not RPM or percent)"
        );
        ensure!(
            *level >= minimum,
            "point {point} is {level}: the firmware minimum at this point is {minimum}"
        );
        if i > 0 && *level < curve[i - 1] {
            bail!(
                "point {point} is {level}, lower than point {i} ({}): each level must be equal to or higher than \
                 the level before",
                curve[i - 1]
            );
        }
    }
    Ok(curve)
}

fn control<'a>(m: &'a Machine) -> Result<&'a FanControl> {
    m.profile.fan.as_ref().context(
        "the profile has no fan block: legionctl does not control the fans of this machine",
    )
}

fn mode(m: &Machine) -> Result<Option<String>> {
    m.root.read(&m.profile.paths.platform_profile)
}

/// Why the fan table and full-speed calls are refused, or `None` when the profile marks them verified.
fn curve_refusal(m: &Machine) -> Option<String> {
    (!m.profile.fan.as_ref().is_some_and(|f| f.curve_verified)).then(|| {
        "the fan table and full speed are not available on this machine: their writes are not tested on this \
         firmware. Use --dry-run to see the writes"
            .to_string()
    })
}

/// Refuse a fan table or full-speed call that the profile does not mark verified (the daemon, before it carries
/// out a plan; a dry run shows the call).
///
/// # Errors
/// The profile does not set `fan.curve_verified`.
pub fn require_verified(m: &Machine) -> Result<()> {
    curve_refusal(m).map_or(Ok(()), |why| Err(anyhow::anyhow!(why)))
}

fn require_custom(m: &Machine, what: &str) -> Result<()> {
    let now = mode(m)?;
    ensure!(
        now.as_deref() == Some(CUSTOM),
        "{what} works in custom mode only, and the mode is now {}: the firmware resets the fan table in the other \
         modes. Set custom mode first: write custom to {} (legionctl keeps it as your choice, as for Fn+Q)",
        now.map_or("unknown".into(), |m| format!("'{m}'")),
        m.profile.paths.platform_profile
    );
    Ok(())
}

/// The module's file of table point `i` (0-based).
fn point_file(i: usize) -> String {
    format!("pwm1_auto_point{}_pwm", i + 1)
}

/// The table the firmware has now, from the module's `pwm1_auto_point1_pwm` .. `pwm1_auto_point10_pwm`.
///
/// # Errors
/// The module is not loaded, a point cannot be read, or a level is outside 1-10.
pub fn read_curve(root: &SysRoot) -> Result<[u8; POINTS]> {
    let hwmon = legionctl_wmi(root)?;
    let mut levels = [0; POINTS];
    for (i, level) in levels.iter_mut().enumerate() {
        let value = read_number(root, &hwmon, &point_file(i))?;
        *level = u8::try_from(value)
            .ok()
            .filter(|l| (1..=10).contains(l))
            .with_context(|| format!("{} is {value}, not a level 1-10", point_file(i)))?;
    }
    Ok(levels)
}

/// The user's curve from the ledger.
fn chosen(m: &Machine) -> Result<Option<[u8; POINTS]>> {
    let Some(kept) = m.ledger.fan(FanKey::Curve)? else {
        return Ok(None);
    };
    let levels = kept
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<Vec<u8>, _>>()
        .map_err(anyhow::Error::from)
        .and_then(|levels| validate(&levels))
        .with_context(|| {
            format!("the saved fan curve '{kept}' is not valid (legionctl fan curve reset)")
        })?;
    Ok(Some(levels))
}

/// Plan `fan curve set`: check the levels, then the mode; write the table and keep it as the user's curve.
///
/// # Errors
/// No fan block in the profile, an invalid curve (see [`validate`]), or not in custom mode.
pub fn set_curve(m: &Machine, levels: &[u8]) -> Result<Vec<Step>> {
    control(m)?;
    let curve = validate(levels)?;
    legionctl_wmi(m.root)?;
    require_custom(m, "the fan curve")?;
    Ok(vec![
        Step::FanCurve(curve),
        Step::Fan {
            key: FanKey::Curve,
            value: Some(text(&curve)),
        },
    ])
}

/// Plan `fan curve reset`: forget the user's curve; in custom mode write the profile's default curve now.
///
/// # Errors
/// No fan block in the profile, or its default curve is not valid.
pub fn reset_curve(m: &Machine) -> Result<Vec<Step>> {
    let default =
        validate(&control(m)?.default_curve).context("the profile's fan.default_curve")?;
    legionctl_wmi(m.root)?;
    let mut steps = vec![Step::Fan {
        key: FanKey::Curve,
        value: None,
    }];
    if mode(m)?.as_deref() == Some(CUSTOM) {
        steps.push(Step::FanCurve(default));
    } else {
        steps.push(Step::Info(format!(
            "the default fan curve {} applies at the next entry to custom mode",
            text(&default)
        )));
    }
    Ok(steps)
}

/// Plan `fan max on|off`; the daemon keeps the state and sets it again after resume and at start.
///
/// # Errors
/// No fan block in the profile.
pub fn max(m: &Machine, on: bool) -> Result<Vec<Step>> {
    control(m)?;
    legionctl_wmi(m.root)?;
    Ok(vec![
        Step::Hwmon {
            file: MAX_MODE,
            value: u8::from(on),
        },
        Step::Fan {
            key: FanKey::Max,
            value: on.then(|| "on".into()),
        },
    ])
}

/// Plan `fan clean on|off` (full speed, for dust cleaning).
///
/// # Errors
/// No fan block in the profile, or not in custom mode.
pub fn clean(m: &Machine, on: bool) -> Result<Vec<Step>> {
    control(m)?;
    legionctl_wmi(m.root)?;
    require_custom(m, "full speed")?;
    Ok(vec![Step::Hwmon {
        file: ENABLE,
        value: if on { 0 } else { 2 },
    }])
}

/// The EC fan lock writes, without the profile's gate (a dry run shows them).
#[must_use]
pub fn lock_steps(on: bool) -> Vec<Step> {
    vec![Step::Ec {
        addr: EC_FAN_LOCK,
        value: if on { EC_LOCKED } else { 0 },
    }]
}

/// Why the EC fan lock is refused, or `None` when the profile marks it verified.
#[must_use]
pub fn lock_refusal(m: &Machine) -> Option<String> {
    (!m.profile.fan.as_ref().is_some_and(|f| f.ec_fan_lock_verified)).then(|| {
        format!(
            "the EC fan lock is not available on this machine: it writes EC RAM 0x{EC_FAN_LOCK:04X} directly, this \
             is not tested on this EC, and a wrong write can stop fan control. Use --dry-run to see the writes"
        )
    })
}

/// Plan `fan lock on|off`.
///
/// # Errors
/// The profile does not mark the EC fan lock verified.
pub fn lock(m: &Machine, on: bool) -> Result<Vec<Step>> {
    if let Some(why) = lock_refusal(m) {
        bail!(why);
    }
    Ok(lock_steps(on))
}

/// What the daemon sets again after an event: fan max mode when the module does not have it (at start: a new module
/// has it off; after resume the module set it again itself, `with_max`), and the user's curve (else the default) when
/// the machine is in custom mode after `plan` (the module sets again only a table written since it loaded). Without
/// the module, a warning says what is not set again; the rest of the event's plan goes on.
///
/// # Errors
/// The firmware mode or the module's fan max mode cannot be read.
pub fn reassert(m: &Machine, with_max: bool, plan: &[Step]) -> Result<Vec<Step>> {
    let Some(fan) = m.profile.fan.as_ref() else {
        return Ok(vec![]);
    };
    let max = with_max && m.ledger.fan(FanKey::Max)?.is_some();
    let planned = plan.iter().rev().find_map(|s| match s {
        Step::Write { path, value } if *path == m.profile.paths.platform_profile => {
            Some(value.clone())
        }
        _ => None,
    });
    let after = match planned {
        Some(value) => Some(value),
        None => mode(m)?,
    };
    let curve_due = after.as_deref() == Some(CUSTOM) && curve_refusal(m).is_none();
    if !max && !curve_due {
        return Ok(vec![]);
    }
    let hwmon = match legionctl_wmi(m.root) {
        Ok(hwmon) => hwmon,
        Err(e) => {
            return Ok(vec![Step::Warn(format!(
                "the fan controls are not set again: {e:#}"
            ))]);
        }
    };
    let mut steps = Vec::new();
    if max && read_number(m.root, &hwmon, MAX_MODE)? != 1 {
        steps.push(Step::Hwmon {
            file: MAX_MODE,
            value: 1,
        });
    }
    if curve_due {
        let curve = match chosen(m)? {
            Some(curve) => curve,
            None => validate(&fan.default_curve).context("the profile's fan.default_curve")?,
        };
        steps.push(Step::FanCurve(curve));
    }
    Ok(steps)
}

/// The fan controls now.
///
/// # Errors
/// The module is not loaded, or the firmware mode or a fan mode file cannot be read.
pub fn state(m: &Machine) -> Result<FanState> {
    let hwmon = legionctl_wmi(m.root)?;
    let curve = read_curve(m.root);
    Ok(FanState {
        mode: mode(m)?,
        curve_error: curve.as_ref().err().map(|e| format!("{e:#}")),
        curve: curve.ok().map(Vec::from),
        chosen: chosen(m)?.map(Vec::from),
        default_curve: m
            .profile
            .fan
            .as_ref()
            .map(|f| f.default_curve.clone())
            .unwrap_or_default(),
        max: read_number(m.root, &hwmon, MAX_MODE)? == 1,
        full_speed: read_number(m.root, &hwmon, ENABLE)? == 0,
        fan1_rpm: RPM[0].to_vec(),
        fan2_rpm: RPM[1].to_vec(),
        ec_fan_lock_verified: lock_refusal(m).is_none(),
    })
}

/// The point writes from table `from` to `to` in two passes, so that the table stays valid after each write (the
/// module refuses a write that makes it invalid): from point 10 down, the higher of the two levels; then from point 1
/// up, the new level. Points that do not change are not written.
fn point_writes(from: &[u8; POINTS], to: &[u8; POINTS]) -> Vec<(usize, u8)> {
    let mut table = *from;
    let mut writes = Vec::new();
    let passes = (0..POINTS)
        .rev()
        .map(|i| (i, from[i].max(to[i])))
        .chain((0..POINTS).map(|i| (i, to[i])));
    for (i, level) in passes {
        if table[i] != level {
            table[i] = level;
            writes.push((i, level));
        }
    }
    writes
}

/// Write the table point by point through the module and read it back; the line says what the firmware has after.
///
/// # Errors
/// The module is not loaded, a write fails (the module refuses it: `EBUSY` outside custom mode, `EINVAL` for an
/// invalid table, `EIO` when the firmware does not keep it), or the table does not read back as written.
pub fn write_curve(root: &SysRoot, levels: &[u8; POINTS]) -> Result<String> {
    let hwmon = legionctl_wmi(root)?;
    let before = read_curve(root).context("reading the fan curve before the write")?;
    let writes = point_writes(&before, levels);
    for (i, level) in &writes {
        let file = hwmon.join(point_file(*i));
        std::fs::write(root.path(&file), format!("{level}\n"))
            .with_context(|| format!("writing {level} to {}", file.display()))?;
    }
    let after = read_curve(root).with_context(|| {
        format!(
            "the fan curve {} was written but cannot be read back",
            text(levels)
        )
    })?;
    ensure!(
        after == *levels,
        "the fan curve read back as {} after writing {}",
        text(&after),
        text(levels)
    );
    // the writes in their order (point:level), so a refused write on the real machine can be traced
    let order: Vec<String> = writes
        .iter()
        .map(|(i, l)| format!("{}:{l}", i + 1))
        .collect();
    Ok(format!(
        "fan curve is now {} (was {}; writes {})",
        text(levels),
        text(&before),
        if order.is_empty() {
            "none".into()
        } else {
            order.join(" ")
        }
    ))
}

/// Write `value` to `file` of the module's hwmon device and read it back; the line says what it was before.
///
/// # Errors
/// The module is not loaded, the module refuses the write (`EINVAL`, `EBUSY`), or the file reads back different.
pub fn write_hwmon(root: &SysRoot, file: &str, value: u8) -> Result<String> {
    let hwmon = legionctl_wmi(root)?;
    let before = read_number(root, &hwmon, file)?;
    let path = hwmon.join(file);
    std::fs::write(root.path(&path), format!("{value}\n"))
        .with_context(|| format!("writing {value} to {}", path.display()))?;
    let after = read_number(root, &hwmon, file)?;
    ensure!(
        after == u64::from(value),
        "{file} of hwmon legionctl_wmi read back as {after} after writing {value}"
    );
    Ok(format!(
        "set {file} of hwmon legionctl_wmi to {value} (was {before})"
    ))
}

/// The port writes of one EC RAM byte: the address high byte (index 0x11), low byte (0x10), then the value (0x12).
#[must_use]
pub fn ec_port_writes(addr: u16, value: u8) -> [(u64, u8); 12] {
    let [high, low] = addr.to_be_bytes();
    let mut writes = [(0, 0); 12];
    for (i, (register, byte)) in [(0x11, high), (0x10, low), (0x12, value)]
        .into_iter()
        .enumerate()
    {
        writes[4 * i..4 * i + 4].copy_from_slice(&[
            (EC_INDEX, 0x2E),
            (EC_DATA, register),
            (EC_INDEX, 0x2F),
            (EC_DATA, byte),
        ]);
    }
    writes
}

/// Write one EC RAM byte through `/dev/port`.
///
/// # Errors
/// `/dev/port` is not writable (no root).
pub fn ec_write(root: &SysRoot, addr: u16, value: u8) -> Result<()> {
    let port = std::fs::OpenOptions::new()
        .write(true)
        .open(root.path("/dev/port"))
        .context("opening /dev/port")?;
    for (at, byte) in ec_port_writes(addr, value) {
        port.write_all_at(&[byte], at)
            .with_context(|| format!("writing port {at:#x}"))?;
    }
    Ok(())
}
