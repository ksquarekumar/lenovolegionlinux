//! Thermal trips (note 04): loads `bpf/thermal.bpf.c` and reads its count per thermal zone.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use aya::maps::{HashMap, MapData};
use legionctl_machine::sysroot::SysRoot;

pub struct Thermal {
    trips: HashMap<MapData, u32, u64>,
}

impl Thermal {
    /// The pinned program of this build, else load, attach and pin it.
    ///
    /// # Errors
    /// No BPF permission, no BTF, the verifier refuses the program (its message is in the error), or a pin fails.
    pub fn start(root: &SysRoot) -> Result<Thermal> {
        let object = aya::include_bytes_aligned!(concat!(env!("OUT_DIR"), "/thermal.bpf.o"));
        let [trips] = crate::pins::attach(
            root,
            "thermal",
            object,
            &[("trip", "thermal_zone_trip")],
            &["trips"],
        )?
        .try_into()
        .ok()
        .context("one map")?;
        Ok(Thermal {
            trips: HashMap::try_from(trips)?,
        })
    }

    /// The trips of each zone since the program was attached, by the zone's type (`x86_pkg_temp 3`); the zone ids are
    /// the running kernel's, so its sysfs names them.
    ///
    /// # Errors
    /// The map cannot be read.
    pub fn per_zone(&self) -> Result<BTreeMap<String, u64>> {
        self.trips
            .iter()
            .map(|entry| {
                let (id, n) = entry?;
                let kind =
                    std::fs::read_to_string(format!("/sys/class/thermal/thermal_zone{id}/type"))
                        .map(|t| t.trim().to_string())
                        .unwrap_or_default();
                Ok((format!("{kind} {id}").trim().to_string(), n))
            })
            .collect()
    }
}
