//! Fan control of this model (docs/design/02-platform.md, Fan control): the fan curve, fan max mode and full speed
//! through the Lenovo WMI methods (`acpi_call`), and the EC fan lock (a raw EC write) only when the profile marks it
//! verified. Plans are steps like the power policy's: printed in a dry run, carried out by `legionctl-system`.

use std::fmt::Write;
use std::os::unix::fs::FileExt;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::firmware::{acpi_call, parse_acpi_buffer};
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

/// `FAN_METHOD` `Fan_Get_Table` (id 5), no input, as the kernel's WMI calls it.
const GET_TABLE: &str = "\\_SB_.GZFD.WMAB 0 5";
/// `GameZone` `WMAA` id 0x0D: fan max mode, one byte (1 on, 0 off).
const MAX_MODE: &str = "\\_SB_.GZFD.WMAA 0 13 b";
/// Other Method `SetFeatureValue` (18), feature 0x04020000 (full speed), then the value; both u32 little endian.
const FULL_SPEED: &str = "\\_SB_.GZFD.WMAE 0 18 b00000204";
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
    /// Fan max mode as the daemon set it (the firmware cannot report it).
    pub max: bool,
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

fn mode(m: &Machine) -> Option<String> {
    m.root.read(&m.profile.paths.platform_profile)
}

/// Why the fan table and full-speed calls are refused, or `None` when the profile marks them verified.
fn curve_refusal(m: &Machine) -> Option<String> {
    (!m.profile.fan.as_ref().is_some_and(|f| f.curve_verified)).then(|| {
        "the fan table calls (WMAB 5/6, WMAE 18) are not verified on this firmware: legionctl makes none of them until \
         the profile sets fan.curve_verified to true (todo-17; 02-platform, Fan control, says how to verify). Use \
         --dry-run to see the call"
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
    let now = mode(m);
    ensure!(
        now.as_deref() == Some(CUSTOM),
        "{what} works in custom mode only, and the mode is now {}: the firmware resets the fan table in the other \
         modes. Set custom mode first: write custom to {} (legionctl keeps it as your choice, as for Fn+Q)",
        now.map_or("unknown".into(), |m| format!("'{m}'")),
        m.profile.paths.platform_profile
    );
    Ok(())
}

/// The `Fan_Set_Table` call: a 0x40-byte buffer with level i at 0x06 + 2·i (FSS0-FSS9); the firmware ignores the
/// rest.
#[must_use]
pub fn set_table_call(levels: &[u8; POINTS]) -> String {
    let mut buffer = [0u8; 0x40];
    for (i, level) in levels.iter().enumerate() {
        buffer[0x06 + 2 * i] = *level;
    }
    let mut call = "\\_SB_.GZFD.WMAB 0 6 b".to_string();
    for b in buffer {
        // writing to a String cannot fail
        let _ = write!(call, "{b:02x}");
    }
    call
}

fn max_call(on: bool) -> String {
    format!("{MAX_MODE}{:02x}", u8::from(on))
}

/// The table the firmware has now: `Fan_Get_Table` returns u32 FSFL, then FSS0-FSS9 as u32 (little endian).
///
/// # Errors
/// The call fails, or the reply is not a table of 10 levels 1-10.
pub fn read_curve(root: &SysRoot) -> Result<[u8; POINTS]> {
    let reply = acpi_call(root, GET_TABLE)?;
    let bytes = parse_acpi_buffer(&reply)
        .filter(|b| b.len() >= 4 + 4 * POINTS)
        .with_context(|| format!("{GET_TABLE} did not return a fan table: {reply}"))?;
    let levels: [u8; POINTS] = std::array::from_fn(|i| bytes[4 + 4 * i]);
    ensure!(
        levels.iter().all(|l| (1..=10).contains(l)),
        "{GET_TABLE} returned levels outside 1-10: {reply}"
    );
    Ok(levels)
}

/// The user's curve from the ledger.
fn chosen(m: &Machine) -> Option<[u8; POINTS]> {
    let kept = m.ledger.fan(FanKey::Curve)?;
    let levels: Vec<u8> = kept
        .split_whitespace()
        .filter_map(|l| l.parse().ok())
        .collect();
    validate(&levels).ok()
}

/// Plan `fan curve set`: check the levels, then the mode; write the table and keep it as the user's curve.
///
/// # Errors
/// No fan block in the profile, an invalid curve (see [`validate`]), or not in custom mode.
pub fn set_curve(m: &Machine, levels: &[u8]) -> Result<Vec<Step>> {
    control(m)?;
    let curve = validate(levels)?;
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
    let mut steps = vec![Step::Fan {
        key: FanKey::Curve,
        value: None,
    }];
    if mode(m).as_deref() == Some(CUSTOM) {
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
    Ok(vec![
        Step::Acpi(max_call(on)),
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
    require_custom(m, "full speed")?;
    Ok(vec![Step::Acpi(format!(
        "{FULL_SPEED}{:02x}000000",
        u8::from(on)
    ))])
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
            "the EC fan lock is not verified for this EC: it writes EC RAM 0x{EC_FAN_LOCK:04X} through port I/O, and an \
             incorrect write can stop fan control. legionctl does not do it until the profile sets \
             fan.ec_fan_lock_verified to true. Use --dry-run to see the writes"
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

/// What the daemon sets again after an event: fan max mode (after resume and at start, `with_max`), and the user's
/// curve (else the default) when the machine is in custom mode after `plan`.
#[must_use]
pub fn reassert(m: &Machine, with_max: bool, plan: &[Step]) -> Vec<Step> {
    let Some(fan) = m.profile.fan.as_ref() else {
        return vec![];
    };
    let mut steps = Vec::new();
    if with_max && m.ledger.fan(FanKey::Max).is_some() {
        steps.push(Step::Acpi(max_call(true)));
    }
    let after = plan
        .iter()
        .rev()
        .find_map(|s| match s {
            Step::Write { path, value } if *path == m.profile.paths.platform_profile => {
                Some(value.clone())
            }
            _ => None,
        })
        .or_else(|| mode(m));
    if after.as_deref() == Some(CUSTOM)
        && curve_refusal(m).is_none()
        && let Some(curve) = chosen(m).or_else(|| validate(&fan.default_curve).ok())
    {
        steps.push(Step::FanCurve(curve));
    }
    steps
}

/// The fan controls now.
#[must_use]
pub fn state(m: &Machine) -> FanState {
    let curve =
        curve_refusal(m).map_or_else(|| read_curve(m.root), |why| Err(anyhow::anyhow!(why)));
    FanState {
        mode: mode(m),
        curve_error: curve.as_ref().err().map(|e| format!("{e:#}")),
        curve: curve.ok().map(Vec::from),
        chosen: chosen(m).map(Vec::from),
        default_curve: m
            .profile
            .fan
            .as_ref()
            .map(|f| f.default_curve.clone())
            .unwrap_or_default(),
        max: m.ledger.fan(FanKey::Max).is_some(),
        fan1_rpm: RPM[0].to_vec(),
        fan2_rpm: RPM[1].to_vec(),
        ec_fan_lock_verified: lock_refusal(m).is_none(),
    }
}

/// Write the table and read it back; the lines say what the firmware has after.
#[must_use]
pub fn write_curve(root: &SysRoot, levels: &[u8; POINTS]) -> Vec<String> {
    let before = read_curve(root).map_or_else(|_| "unknown".into(), |c| text(&c));
    if let Err(e) = acpi_call(root, &set_table_call(levels)) {
        return vec![format!("warning: the fan curve was not written: {e:#}")];
    }
    match read_curve(root) {
        Ok(after) if after == *levels => {
            vec![format!("fan curve is now {} (was {before})", text(levels))]
        }
        Ok(after) => vec![format!(
            "warning: the fan curve read back as {} after writing {}",
            text(&after),
            text(levels)
        )],
        Err(e) => vec![format!(
            "warning: the fan curve {} was written but cannot be read back: {e:#}",
            text(levels)
        )],
    }
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
