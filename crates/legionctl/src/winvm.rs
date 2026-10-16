//! `legionctl windows-vm` (note 05): the Windows VM's runner, as the user service `legionctl-windows-vm` that
//! `windows vm start` starts. It checks the free memory, has `legionctl-system` make the host ready (the Windows disk
//! on vfio-pci, USB devices, the memory lock, the MSDM table), starts swtpm and QEMU, and gives the host back when
//! QEMU ends. Its state file tells `windows vm start` and `status` how it goes.

use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use legionctl_api::client::Client;
use legionctl_api::{Empty, WindowsVmBeginIn, op};
use legionctl_core::config::{Dirs, WindowsVmConfig, resolve};

/// A child process that ends with the runner.
struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `legionctl windows-vm`: run the Windows VM until it shuts down.
///
/// # Errors
/// The start fails (the state file says why).
pub fn run() -> Result<()> {
    let dir = legionctl_core::session::windows_root();
    std::fs::create_dir_all(&dir)?;
    let state = dir.join("state");
    let result = vm(&dir, &state);
    let last = match &result {
        Ok(()) => "stopped".to_string(),
        Err(e) => format!("failed: {e:#}"),
    };
    std::fs::write(&state, last)?;
    result
}

fn vm(dir: &Path, state: &Path) -> Result<()> {
    let config = resolve(&Dirs::from_env(), None)?.config.windows.vm;
    enough_memory(&config)?;
    let runtime = tokio::runtime::Runtime::new()?;
    // a SIGTERM (the unit's stop) must not end the runner before it gives the host back: the stop sends the power
    // button, and the runner ends when QEMU does
    let _term = runtime.block_on(async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    })?;
    let client = Client::new("cli")?;
    let begun = runtime.block_on(
        client.call::<op::InternalWindowsVmBegin>(&WindowsVmBeginIn {
            pid: std::process::id(),
            memlock_mib: config.memory_mib + 1024,
        }),
    )?;
    // from here on the host is changed: give it back however QEMU ends
    let ran = boot(dir, state, &config, &begun);
    let back = runtime.block_on(client.call::<op::InternalWindowsVmEnd>(&Empty {}));
    ran.and(back.map(|_| ()))
}

/// Refuse a start that would leave Linux less than `reserve_mib` (VFIO locks all of the VM's memory).
fn enough_memory(config: &WindowsVmConfig) -> Result<()> {
    let meminfo = std::fs::read_to_string("/proc/meminfo")?;
    let available_mib = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))
        .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        .context("no MemAvailable in /proc/meminfo")?
        >> 10;
    let needed = config.memory_mib + config.reserve_mib;
    if available_mib >= needed {
        return Ok(());
    }
    let mut largest: Vec<(u64, String)> = std::fs::read_dir("/proc")?
        .flatten()
        .filter_map(|e| {
            let status = std::fs::read_to_string(e.path().join("status")).ok()?;
            let field = |name: &str| {
                status
                    .lines()
                    .find_map(|l| l.strip_prefix(name))
                    .map(|v| v.trim().to_string())
            };
            let rss = field("VmRSS:")?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()?
                >> 10;
            Some((rss, field("Name:")?))
        })
        .collect();
    largest.sort_unstable_by(|a, b| b.cmp(a));
    let names: Vec<String> = largest
        .iter()
        .take(3)
        .map(|(mib, name)| format!("{name} {mib} MiB"))
        .collect();
    anyhow::bail!(
        "{available_mib} MiB is free, and the VM needs {} MiB plus {} MiB for Linux (windows.vm.memory_mib, \
         reserve_mib); the largest processes: {}",
        config.memory_mib,
        config.reserve_mib,
        names.join(", ")
    )
}

/// A value for a QEMU option list (a comma is written twice).
fn opt(value: &str) -> String {
    value.replace(',', ",,")
}

/// The host's SMBIOS identity for the VM (Windows' activation checks it).
///
/// # Errors
/// A DMI file cannot be read: an empty identity would break the activation.
fn smbios() -> Result<Vec<String>> {
    let dmi = |f: &str| -> Result<String> {
        let path = format!("/sys/class/dmi/id/{f}");
        Ok(opt(std::fs::read_to_string(&path)
            .with_context(|| format!("reading {path}"))?
            .trim()))
    };
    Ok(vec![
        "-smbios".into(),
        format!(
            "type=1,manufacturer={},product={},version={},family={}",
            dmi("sys_vendor")?,
            dmi("product_name")?,
            dmi("product_version")?,
            dmi("product_family")?
        ),
        "-smbios".into(),
        format!(
            "type=2,manufacturer={},product={}",
            dmi("board_vendor")?,
            dmi("board_name")?
        ),
    ])
}

/// The QEMU command of the VM in `dir`; with shares, its memory is shareable (virtiofs needs that).
fn qemu_args(dir: &Path, config: &WindowsVmConfig, devices: &[String]) -> Result<Vec<String>> {
    let machine = if config.shares.is_empty() {
        "q35,accel=kvm,smm=off".to_string()
    } else {
        "q35,accel=kvm,smm=off,memory-backend=mem".to_string()
    };
    let path = |name: &str| opt(&dir.join(name).to_string_lossy());
    let mut args: Vec<String> = vec![
        "taskset".into(),
        "-c".into(),
        config.host_cpus.clone(),
        "qemu-system-x86_64".into(),
        "-name".into(),
        "windows".into(),
        "-nodefaults".into(),
        // no SMM: this kernel's KVM resets a guest with SMM (note 18)
        "-machine".into(),
        machine,
        "-cpu".into(),
        "host,hv_relaxed,hv_vapic,hv_spinlocks=0x1fff,hv_time,hv_vpindex,hv_synic,hv_stimer,hv_frequencies,hv_reset,hv_tlbflush,hv_ipi".into(),
        "-smp".into(),
        config.vcpus.to_string(),
        "-m".into(),
        format!("{}M", config.memory_mib),
        "-drive".into(),
        format!("if=pflash,format=raw,readonly=on,file={}", opt(&config.ovmf_code)),
        "-drive".into(),
        format!("if=pflash,format=raw,file={}", path("OVMF_VARS.fd")),
        "-chardev".into(),
        format!("socket,id=tpm,path={}", path("tpm.sock")),
        "-tpmdev".into(),
        "emulator,id=tpm0,chardev=tpm".into(),
        "-device".into(),
        "tpm-crb,tpmdev=tpm0".into(),
        "-acpitable".into(),
        format!("file={}", path("msdm.bin")),
        "-nic".into(),
        "user,model=e1000e".into(),
        // the display until a GPU comes (gpu give), and the one Windows falls back to
        "-device".into(),
        "qxl-vga".into(),
        "-spice".into(),
        format!("unix=on,addr={},disable-ticketing=on", path("spice.sock")),
        "-device".into(),
        "virtio-serial-pci".into(),
        "-chardev".into(),
        "spicevmc,id=vdagent,name=vdagent".into(),
        "-device".into(),
        "virtserialport,chardev=vdagent,name=com.redhat.spice.0".into(),
        "-audiodev".into(),
        "pipewire,id=sound".into(),
        "-device".into(),
        "ich9-intel-hda".into(),
        "-device".into(),
        "hda-duplex,audiodev=sound".into(),
        // a free PCIe slot for a GPU that comes while Windows runs
        "-device".into(),
        "pcie-root-port,id=gpu-port,chassis=1,slot=1".into(),
        "-device".into(),
        "qemu-xhci,id=xhci".into(),
        "-qmp".into(),
        format!("unix:{},server=on,wait=off", path("qmp.sock")),
        "-display".into(),
        "none".into(),
        "-monitor".into(),
        "none".into(),
    ];
    if !config.shares.is_empty() {
        args.extend([
            "-object".into(),
            format!(
                "memory-backend-memfd,id=mem,size={}M,share=on",
                config.memory_mib
            ),
        ]);
    }
    if config.looking_glass_mib > 0 {
        // Windows' frames for Looking Glass (`windows vm view` while Windows has the GPU)
        args.extend([
            "-object".into(),
            format!(
                "memory-backend-file,id=lg,mem-path={},size={}M,share=on",
                opt(&legionctl_core::session::looking_glass_file().to_string_lossy()),
                config.looking_glass_mib
            ),
            "-device".into(),
            "ivshmem-plain,memdev=lg".into(),
        ]);
    }
    args.extend(smbios()?);
    for device in devices {
        args.extend(["-device".into(), format!("vfio-pci,host={device}")]);
    }
    for id in &config.usb {
        let (vendor, product) = id
            .split_once(':')
            .with_context(|| format!("windows.vm.usb {id}: not <vendor>:<product>"))?;
        args.extend([
            "-device".into(),
            format!("usb-host,bus=xhci.0,vendorid=0x{vendor},productid=0x{product}"),
        ]);
    }
    Ok(args)
}

/// Wait until `path` exists (a socket a child makes), or the child ends.
fn wait_for(path: &Path, child: &mut Child, what: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("{what} ended ({status}) before {} appeared", path.display());
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "{} did not appear in 10 s",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn boot(
    dir: &Path,
    state: &Path,
    config: &WindowsVmConfig,
    begun: &legionctl_api::WindowsVmBegun,
) -> Result<()> {
    std::fs::write(dir.join("msdm.bin"), &begun.msdm)?;
    let vars = dir.join("OVMF_VARS.fd");
    if !vars.exists() {
        std::fs::copy(&config.ovmf_vars, &vars)
            .with_context(|| format!("copying {} (windows.vm.ovmf_vars)", config.ovmf_vars))?;
    }
    let tpm_dir = dir.join("tpm");
    std::fs::create_dir_all(&tpm_dir)?;
    let tpm_socket = dir.join("tpm.sock");
    let _ = std::fs::remove_file(&tpm_socket);
    let mut tpm = Kill(
        Command::new("swtpm")
            .args(["socket", "--tpm2", "--terminate", "--tpmstate"])
            .arg(format!("dir={}", tpm_dir.display()))
            .arg("--ctrl")
            .arg(format!("type=unixio,path={}", tpm_socket.display()))
            .spawn()
            .context("starting swtpm")?,
    );
    wait_for(&tpm_socket, &mut tpm.0, "swtpm")?;
    let args = qemu_args(dir, config, &begun.devices)?;
    let mut command = Command::new(&args[0]);
    command.args(&args[1..]);
    let shares: Vec<(String, std::path::PathBuf, bool)> = config
        .shares
        .iter()
        .map(|folder| {
            let path = legionctl_core::config::expand_home(folder)?;
            let tag = path
                .file_name()
                .map_or_else(|| "share".into(), |n| n.to_string_lossy().into_owned());
            Ok((tag, path, false))
        })
        .collect::<Result<_>>()?;
    let virtiofsd = resolve(&Dirs::from_env(), None)?
        .config
        .session
        .vm
        .virtiofsd;
    let _daemons = crate::vm::share(
        &mut command,
        dir,
        &virtiofsd,
        &shares.iter().collect::<Vec<_>>(),
    )?;
    let mut qemu = Kill(command.spawn().context("starting qemu-system-x86_64")?);
    let spice = dir.join("spice.sock");
    std::fs::write(
        state,
        format!(
            "running\ndisplay: remote-viewer spice+unix://{}\n{}",
            spice.display(),
            begun.lines.join("\n")
        ),
    )?;
    let status = qemu.0.wait()?;
    anyhow::ensure!(status.success(), "QEMU ended with {status}");
    Ok(())
}

/// `legionctl windows-vm-stop` (the unit's stop): the power button through QMP; after 2 minutes the VM is ended. Waits
/// until the runner (`$MAINPID`) gave the host back and ended.
///
/// # Errors
/// QMP refuses, or the runner does not end.
pub fn stop() -> Result<()> {
    let socket = legionctl_core::session::windows_root().join("qmp.sock");
    // systemd sets it for ExecStop; without it the stop would return before the runner gave the GPU back
    let runner = std::env::var("MAINPID")
        .ok()
        .filter(|p| !p.is_empty())
        .context("MAINPID is not set: windows-vm-stop runs as the unit's ExecStop")?;
    let alive = || Path::new("/proc").join(&runner).exists();
    let mut qmp = legionctl_core::qmp::Qmp::connect(&socket)?;
    qmp.execute("system_powerdown", None)?;
    for (limit, last) in [
        (Duration::from_mins(2), "quit"),
        (Duration::from_secs(30), ""),
    ] {
        let deadline = Instant::now() + limit;
        while alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(500));
        }
        if !alive() {
            return Ok(());
        }
        if !last.is_empty() {
            eprintln!("legionctl: Windows did not shut down in 2 minutes: the VM is ended");
            qmp.execute(last, None)?;
        }
    }
    anyhow::bail!("the Windows VM's runner did not end")
}
