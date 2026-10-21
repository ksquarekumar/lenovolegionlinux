//! The static client of `make static` for the tests that run it (e2e.rs container tests, machine.rs).

use std::path::{Path, PathBuf};

/// `bin`, the static client of `make static` (container and VM sessions run it), when no source it was built from is
/// newer than it (F14, plan#todo-225: a stale one ran without notice). Its build ran in a container that saw this
/// checkout at /src, so its dep-info names /src/<path>.
pub fn fresh_static(bin: &Path) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    crate::common::fresh_as(bin, Some(("/src", &root)), "make static")
}
