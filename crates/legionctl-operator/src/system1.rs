//! `org.legionctl.System1`, the machine daemon's D-Bus API: methods (polkit asks for each change) and signals.

#[zbus::proxy(
    default_service = "org.legionctl.System1",
    default_path = "/org/legionctl/System1",
    interface = "org.legionctl.System1"
)]
pub trait System1 {
    fn apply_power(&self, source: &str) -> zbus::Result<Vec<String>>;
    fn set_primary_gpu(&self, choice: &str, release_nvidia: bool) -> zbus::Result<Vec<String>>;
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
    /// Take the NVIDIA GPU from Linux (services stopped, group on vfio-pci).
    fn gpu_release(&self) -> zbus::Result<Vec<String>>;
    /// Give the NVIDIA GPU back to Linux.
    fn gpu_return(&self) -> zbus::Result<Vec<String>>;
    /// The battery's charge mode (standard, conservation, rapid).
    fn set_battery_mode(&self, mode: &str) -> zbus::Result<Vec<String>>;
    /// USB charging while the laptop is off.
    fn set_usb_charging(&self, on: bool) -> zbus::Result<Vec<String>>;
    /// The boot takeover: install, confirm, rebuild or remove.
    fn boot(&self, action: &str) -> zbus::Result<Vec<String>>;
    /// The kernels of the boot takeover: default, try, cmdline (with `line`) or undo, for `name`; or install (the
    /// packages of `name` in the folder `line`).
    fn kernel(&self, action: &str, name: &str, line: &str) -> zbus::Result<Vec<String>>;
    /// The greeter in place of GDM: install or remove.
    fn greeter(&self, action: &str) -> zbus::Result<Vec<String>>;
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
    /// Apply the configset layers again (after a change of your own layers).
    fn reload(&self) -> zbus::Result<Vec<String>>;
    /// Write a system layer (`/etc/legionctl/configsets/<name>.jsonc`); empty content removes it.
    fn set_layer(&self, name: &str, content: &str) -> zbus::Result<Vec<String>>;
    /// Turn a layer on, off, or back to its default.
    fn set_layer_state(&self, name: &str, state: &str) -> zbus::Result<Vec<String>>;
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
