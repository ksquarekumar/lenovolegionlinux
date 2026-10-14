//! Saved sets (todo-85, note 22): a named copy of the configset selection, the on and off state of the layers and the
//! knob holds, which the daemon keeps in `system.db` beside the two tables it copies. A switch writes the two tables
//! back as one change.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// The selection before the last switch: `rules sets switch previous` is the undo.
pub const PREVIOUS: &str = "previous";
/// The selection before a start through a set's boot entry; the next start without one goes back to it.
pub const BEFORE_BOOT: &str = "before-boot";
/// The kernel parameter of a set's boot entry.
pub const PARAMETER: &str = "legionctl.set=";

/// The layers' states (`shipped/20-ac` -> on) and the holds (path -> value), as `system.db` has them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub layers: BTreeMap<String, bool>,
    pub holds: BTreeMap<String, String>,
    /// Each kernel's image has a boot entry for the set.
    #[serde(default)]
    pub boot: bool,
}

/// A name that a person may give a set: it goes on the kernel command line and into the boot menu.
///
/// # Errors
/// The name is empty, has other characters than a-z, 0-9 and '-', or is one of the daemon's own names.
pub fn check_name(name: &str) -> Result<()> {
    anyhow::ensure!(
        !name.is_empty()
            && name.len() <= 32
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "a set's name is 1 to 32 characters a-z, 0-9 and '-', not '{name}'"
    );
    anyhow::ensure!(
        name != PREVIOUS && name != BEFORE_BOOT,
        "'{name}' is legionctl's own name for the selection before a switch or a start"
    );
    Ok(())
}

/// The set that the kernel command line `line` chose (`legionctl.set=<name>`).
#[must_use]
pub fn chosen(line: &str) -> Option<&str> {
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(PARAMETER))
}

/// What a switch from `now` to `to` changes, one line for each layer state and each hold.
#[must_use]
pub fn changes(now: &Selection, to: &Selection) -> Vec<String> {
    let state = |s: Option<&bool>| match s {
        Some(true) => "on",
        Some(false) => "off",
        None => "default",
    };
    let ids: std::collections::BTreeSet<&String> =
        now.layers.keys().chain(to.layers.keys()).collect();
    let mut lines: Vec<String> = ids
        .into_iter()
        .filter(|id| now.layers.get(*id) != to.layers.get(*id))
        .map(|id| {
            format!(
                "layer {id}: {} -> {}",
                state(now.layers.get(id)),
                state(to.layers.get(id))
            )
        })
        .collect();
    for path in now.holds.keys().filter(|p| !to.holds.contains_key(*p)) {
        lines.push(format!("release {path}"));
    }
    for (path, value) in &to.holds {
        if now.holds.get(path) != Some(value) {
            lines.push(format!("hold {path} at {value}"));
        }
    }
    lines
}
