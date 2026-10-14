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
    /// Hold knobs at values against the policy (path -> value).
    fn hold(&self, values: std::collections::BTreeMap<String, String>)
    -> zbus::Result<Vec<String>>;
    /// Release a held knob; the policy takes it again.
    fn release(&self, path: &str) -> zbus::Result<Vec<String>>;
    /// The hash of the interface the daemon was built from (`dbus/org.legionctl.System1.xml`).
    #[zbus(property)]
    fn version(&self) -> zbus::Result<String>;
    /// A sweep (`tune::TuneSpec` as JSON); returns `tune::TuneReport` as JSON.
    fn tune(&self, spec: &str) -> zbus::Result<String>;
    /// Root-only readings as JSON (`telemetry::RootSample`).
    fn sample(&self) -> zbus::Result<String>;
    /// Give one whole IOMMU group to the caller's VM (`pid`: its runner, 0: none; `memlock`: bytes it may lock).
    fn passthrough(&self, devices: &[String], pid: u32, memlock: u64) -> zbus::Result<Vec<String>>;
    /// Give devices back from vfio-pci to their own drivers.
    fn passthrough_end(&self, devices: &[String]) -> zbus::Result<Vec<String>>;
    /// Make the host ready for the caller's Windows VM; the lines and the MSDM table.
    fn windows_vm_begin(
        &self,
        usb: &[String],
        pid: u32,
        memlock: u64,
    ) -> zbus::Result<(Vec<String>, Vec<String>, Vec<u8>)>;
    /// The caller may start and end the Windows VM without a prompt (autostart), or not.
    fn set_windows_autostart(&self, on: bool) -> zbus::Result<Vec<String>>;
    /// Give back what the Windows VM's start changed.
    fn windows_vm_end(&self) -> zbus::Result<Vec<String>>;
    /// The devices that can go to a VM, as JSON (`vfio::Candidate`).
    fn passthroughs(&self) -> zbus::Result<String>;
    /// The fan controls as JSON (`fan::FanState`).
    fn fan_status(&self) -> zbus::Result<String>;
    fn set_fan_curve(&self, levels: &[u8]) -> zbus::Result<Vec<String>>;
    fn reset_fan_curve(&self) -> zbus::Result<Vec<String>>;
    fn set_fan_max(&self, on: bool) -> zbus::Result<Vec<String>>;
    fn set_fan_full_speed(&self, on: bool) -> zbus::Result<Vec<String>>;
    /// The EC fan lock; the daemon refuses it unless the profile marks it verified.
    fn set_fan_lock(&self, on: bool) -> zbus::Result<Vec<String>>;
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
