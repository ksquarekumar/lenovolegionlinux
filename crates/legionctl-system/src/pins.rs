//! Pins (note 04 "Safety", decision D7): each sensor's links and the maps the daemon reads stay under
//! `/sys/fs/bpf/legionctl/<sensor>-<build>`, so a daemon that failed and starts again reuses the attached programs (and
//! the run-queue histogram) of the same build instead of loading them again. A stop (SIGTERM) and the package's removal
//! remove the pins; a load of another build removes the pins of the old one.

use std::path::PathBuf;

use anyhow::{Context, Result};
use aya::maps::{Map, MapData};
use aya::programs::links::FdLink;
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
