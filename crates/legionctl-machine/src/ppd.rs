//! power-profiles-daemon over D-Bus (`org.freedesktop.UPower.PowerProfiles`): its active profile and a change of it.
//! Not `powerprofilesctl`: that is a Python (`PyGObject`) program, and under Python 3.14 it aborted at exit (SIGABRT,
//! a core dump) each time the root daemon ran it.

use std::path::Path;

use anyhow::{Context, Result};

use crate::sysroot::SysRoot;

const NAME: &str = "org.freedesktop.UPower.PowerProfiles";
const OBJECT: &str = "/org/freedesktop/UPower/PowerProfiles";
/// The D-Bus activation file that the power-profiles-daemon package installs: the daemon is on this machine.
const SERVICE: &str =
    "/usr/share/dbus-1/system-services/org.freedesktop.UPower.PowerProfiles.service";

/// A system bus call on a thread of its own: zbus's blocking API runs its own runtime, which a thread inside the
/// caller's tokio runtime may not start.
fn on_bus<T: Send + 'static>(
    call: impl FnOnce(&zbus::blocking::Proxy) -> zbus::Result<T> + Send + 'static,
) -> Result<T> {
    std::thread::spawn(move || -> Result<T> {
        let conn = zbus::blocking::Connection::system().context("connecting to the system bus")?;
        let proxy = zbus::blocking::Proxy::new(&conn, NAME, OBJECT, NAME)?;
        Ok(call(&proxy)?)
    })
    .join()
    .map_err(|_| anyhow::anyhow!("the power-profiles-daemon call panicked"))?
}

/// The active profile (`performance`, `balanced`, `power-saver`); None when power-profiles-daemon is not installed.
///
/// # Errors
/// It is installed and the system bus or the daemon does not answer.
pub fn active(root: &SysRoot) -> Result<Option<String>> {
    if !root.exists(Path::new(SERVICE))? {
        return Ok(None);
    }
    on_bus(|p| p.get_property::<String>("ActiveProfile"))
        .context("reading the power-profiles-daemon profile")
        .map(Some)
}

/// Set the active profile.
///
/// # Errors
/// The system bus or the daemon refuses or does not answer.
pub fn set(profile: &str) -> Result<()> {
    let value = profile.to_string();
    on_bus(move |p| Ok(p.set_property("ActiveProfile", value.as_str())?))
        .with_context(|| format!("setting the power-profiles-daemon profile {profile}"))
}
