//! `org.legionctl.System1`, the machine daemon's D-Bus API: methods (polkit asks for each change) and signals.

#[zbus::proxy(
    default_service = "org.legionctl.System1",
    default_path = "/org/legionctl/System1",
    interface = "org.legionctl.System1"
)]
pub trait System1 {
    fn apply_power(&self, source: &str) -> zbus::Result<Vec<String>>;
    fn set_primary_gpu(&self, choice: &str) -> zbus::Result<Vec<String>>;
    fn cleanup(&self, unload_modules: bool) -> zbus::Result<Vec<String>>;
    fn reboot_windows(&self) -> zbus::Result<Vec<String>>;
    /// The hash of the interface the daemon was built from (`dbus/org.legionctl.System1.xml`).
    #[zbus(property)]
    fn version(&self) -> zbus::Result<String>;
    /// A sweep (`tune::TuneSpec` as JSON); returns `tune::TuneReport` as JSON.
    fn tune(&self, spec: &str) -> zbus::Result<String>;
    /// Root-only readings as JSON (`telemetry::RootSample`).
    fn sample(&self) -> zbus::Result<String>;
    /// Send this connection the program starts of its user's agent sessions (`SessionExec`).
    fn watch_sessions(&self) -> zbus::Result<()>;

    #[zbus(signal)]
    fn session_exec(&self, session: String, pid: u32, argv: Vec<String>) -> zbus::Result<()>;

    #[zbus(signal)]
    fn changed(&self, cause: String, lines: Vec<String>) -> zbus::Result<()>;

    #[zbus(signal)]
    fn key(&self, name: String) -> zbus::Result<()>;

    #[zbus(signal)]
    fn mode_changed(&self, mode: String, fn_q_choice: bool, source: String) -> zbus::Result<()>;

    #[zbus(signal)]
    fn fn_lock(&self, on: bool) -> zbus::Result<()>;
}
