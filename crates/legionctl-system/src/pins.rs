//! Pins (note 04 "Safety", decision D7): each sensor's links and the maps the daemon reads stay under
//! `/sys/fs/bpf/legionctl/<sensor>-<build>`, so a daemon that failed and starts again reuses the attached programs (and
//! the run-queue histogram) of the same build instead of loading them again. A stop (SIGTERM) and the package's removal
//! remove the pins; a load of another build removes the pins of the old one.

use std::path::PathBuf;

use anyhow::{Context, Result};
use aya::maps::{Map, MapData};
use aya::programs::BtfTracePoint;
use aya::programs::links::FdLink;
use aya::{Btf, Ebpf};
use legionctl_machine::sysroot::SysRoot;

const PINS: &str = "/sys/fs/bpf/legionctl";

pub struct Pins {
    dir: PathBuf,
    sensor: &'static str,
}

impl Pins {
    /// The pins of `sensor` built from `object`.
    pub fn of(root: &SysRoot, sensor: &'static str, object: &[u8]) -> Pins {
        Pins {
            dir: root
                .path(PINS)
                .join(format!("{sensor}-{:016x}", legionctl_machine::fnv(object))),
            sensor,
        }
    }

    /// The pinned map `name` of this build. The maps are pinned after the links, so a pinned map means attached
    /// programs.
    pub fn map(&self, name: &str) -> Option<Map> {
        Map::from_map_data(MapData::from_pin(self.dir.join(name)).ok()?).ok()
    }

    /// Pin a new load: the links, then the maps. All earlier pins of the sensor go first (another build, or a load
    /// that stopped before its maps were pinned).
    ///
    /// # Errors
    /// No BPF filesystem, or a pin fails.
    pub fn pin(&self, links: Vec<(&str, FdLink)>, maps: &[(&str, &Map)]) -> Result<()> {
        let parent = self.dir.parent().context("pins have a folder")?;
        let prefix = format!("{}-", self.sensor);
        for entry in std::fs::read_dir(parent).into_iter().flatten().flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                std::fs::remove_dir_all(entry.path())
                    .with_context(|| format!("removing the old pins {}", entry.path().display()))?;
            }
        }
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("making {}", self.dir.display()))?;
        for (name, link) in links {
            let path = self.dir.join(name);
            link.pin(&path)
                .with_context(|| format!("pinning {}", path.display()))?;
        }
        for (name, map) in maps {
            let path = self.dir.join(name);
            map.pin(&path)
                .with_context(|| format!("pinning {}", path.display()))?;
        }
        Ok(())
    }
}

/// The maps `maps` of `sensor`: pinned by this build, else from a new load of `object` with each (program, tracepoint)
/// attached and pinned. A failed daemon's successor so reuses the attached programs.
///
/// # Errors
/// No BPF permission, no BTF, the verifier refuses a program (its message is in the error), or a pin fails.
pub fn attach(
    root: &SysRoot,
    sensor: &'static str,
    object: &[u8],
    programs: &[(&str, &str)],
    maps: &[&str],
) -> Result<Vec<Map>> {
    let pins = Pins::of(root, sensor, object);
    if let Some(pinned) = maps.iter().map(|m| pins.map(m)).collect::<Option<Vec<_>>>() {
        return Ok(pinned);
    }
    let mut bpf = Ebpf::load(object).with_context(|| format!("loading the {sensor} object"))?;
    let btf = Btf::from_sys_fs().context("reading the kernel BTF")?;
    let mut links = Vec::new();
    for (program, tracepoint) in programs {
        let p: &mut BtfTracePoint = bpf
            .program_mut(program)
            .with_context(|| format!("no program {program}"))?
            .try_into()?;
        p.load(tracepoint, &btf)
            .with_context(|| format!("loading {program}"))?;
        let link = p
            .attach()
            .with_context(|| format!("attaching {program} to {tracepoint}"))?;
        links.push((*program, FdLink::from(p.take_link(link)?)));
    }
    let taken = maps
        .iter()
        .map(|m| bpf.take_map(m).with_context(|| format!("no {m} map")))
        .collect::<Result<Vec<_>>>()?;
    let named: Vec<(&str, &Map)> = maps.iter().copied().zip(taken.iter()).collect();
    pins.pin(links, &named)?;
    Ok(taken)
}

/// Remove every pin: the programs detach when the daemon's own handles close.
///
/// # Errors
/// A pin cannot be removed.
pub fn unpin(root: &SysRoot) -> Result<()> {
    match std::fs::remove_dir_all(root.path(PINS)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("removing {PINS}"))
        }
        _ => Ok(()),
    }
}
