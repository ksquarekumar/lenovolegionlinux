//! Sessions in a VM (W5 part 3, note 18). On the host, `legionctl vm` runs a session's VM inside the session's
//! scope: a virtiofs daemon per shared folder, then QEMU on a throwaway layer over the repository's disk, with the
//! agent on the virtio console (this process's stdio); each guest connection to the operator address goes through
//! `legionctl relay` to the operator socket. In the VM, `legionctl guest` (the host's static client from the spec
//! share, which the disk's unit mounts and starts at boot) mounts the shares, gives the agent the operator socket at its usual path, runs the agent as the user on the
//! console, and powers the VM off when the agent ends.

use std::io::IsTerminal;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use legionctl_core::session::{GATEWAY_SOCKET, GuestMount, GuestSpec, VmSpec};
use tokio::io::AsyncWriteExt;

/// The operator and the gateway as the guest reaches them: QEMU's user network runs `legionctl relay` for each
/// connection.
const OPERATOR: &str = "10.0.2.100:80";
const GATEWAY: &str = "10.0.2.100:81";
/// The share with `guest.json` and the static client, and where the disk's unit mounts it (`workspace pack --as vm`).
const SPEC_TAG: &str = "legionctl-spec";
const SPEC_DIR: &str = "/run/legionctl-spec";
/// The agent's channel in the guest: a terminal for an interactive agent; for a headless one a port without a line
/// discipline, which would echo the client's first request back (QEMU holds the input until the guest opens it).
const CONSOLE: &str = "/dev/hvc0";
const PORT: &str = "org.legionctl.agent";
/// A headless agent's stderr in the guest: `legionctl vm`'s own stderr on the host, apart from the agent's output.
const STDERR_PORT: &str = "org.legionctl.stderr";

/// The virtiofs daemons of a VM; they go when the VM ends.
pub(crate) struct Daemons(Vec<Child>);

impl Drop for Daemons {
    fn drop(&mut self) {
        for daemon in &mut self.0 {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
    }
}

/// `legionctl vm <spec> -- <agent>`: run the session's VM until its agent ends.
///
/// # Errors
/// The spec cannot be read, a daemon or QEMU cannot start, or QEMU fails (its console log says why).
pub fn run(spec_path: &Path, argv: Vec<String>) -> Result<()> {
    let mut spec: VmSpec = serde_json::from_str(
        &std::fs::read_to_string(spec_path)
            .with_context(|| format!("reading {}", spec_path.display()))?,
    )?;
    let folder = spec_path.parent().context("the spec has a folder")?;
    let guest_dir = folder.join("vm");
    std::fs::create_dir_all(&guest_dir)?;
    spec.guest.argv = argv;
    spec.guest.interactive = std::io::stdin().is_terminal();
    std::fs::write(
        guest_dir.join("guest.json"),
        serde_json::to_string(&spec.guest)?,
    )?;
    // the guest runs the host's client: the disk never skews from the spec it reads
    let client = guest_dir.join("legionctl");
    std::fs::copy(&spec.client, &client)
        .with_context(|| format!("copying {} for the VM", spec.client.display()))?;
    let console = folder.join("console.log");
    let me = std::env::current_exe()?;
    for path in [me.as_path(), spec.socket.as_path()] {
        anyhow::ensure!(
            !path.to_string_lossy().contains(','),
            "QEMU options cannot hold {} (a comma)",
            path.display()
        );
    }
    let memory = format!("{}M", spec.memory_mib);
    let mut qemu = Command::new("qemu-system-x86_64");
    qemu.args([
        "-machine",
        "q35,accel=kvm,smm=off,memory-backend=mem",
        "-cpu",
        "host",
    ])
    .args(["-smp", &spec.cpus.to_string(), "-m", &memory])
    .arg("-object")
    .arg(format!(
        "memory-backend-memfd,id=mem,size={memory},share=on"
    ))
    .args(["-nodefaults", "-display", "none", "-monitor", "none"])
    .arg("-serial")
    .arg(format!("file:{}", console.display()))
    .arg("-drive")
    .arg(format!(
        "file={},if=virtio,snapshot=on",
        spec.disk.display()
    ))
    .args([
        "-device",
        "virtio-serial-pci",
        "-chardev",
        "stdio,id=agent,signal=off",
    ])
    .args(agent_channel(spec.guest.interactive))
    .arg("-netdev")
    .arg(netdev(&me, &spec))
    .args(["-device", "virtio-net-pci,netdev=net"]);
    // the GPU's whole IOMMU group, bound to vfio-pci for this run (a polkit dialog); given back when this ends
    let gpu = if spec.gpu {
        let devices = nvidia_group()?;
        for device in &devices {
            qemu.arg("-device").arg(format!("vfio-pci,host={device}"));
        }
        Some(Passthrough::begin(devices, spec.memory_mib + 1024)?)
    } else {
        None
    };
    let spec_share = (SPEC_TAG.to_string(), guest_dir, true);
    let shares: Vec<_> = spec.shares.iter().chain([&spec_share]).collect();
    let daemons = share(&mut qemu, folder, &spec.virtiofsd, &shares)?;
    // QEMU's stderr (and a headless agent's, through its port) goes on to this stderr; its last line is the error
    let mut vm = qemu
        .stdin(if spec.guest.interactive {
            Stdio::inherit()
        } else {
            Stdio::piped()
        })
        .stderr(Stdio::piped())
        .spawn()
        .context("running qemu-system-x86_64")?;
    let errors = vm.stderr.take().context("QEMU's stderr")?;
    let last = std::thread::spawn(move || last_line(errors));
    // QEMU closes the guest's channel at the end of its stdin, and a headless agent's writes then block: feed it
    // through a pipe that stays open until the VM ends
    let input = vm.stdin.take();
    if let Some(input) = &input {
        let mut feed = std::fs::File::from(std::os::fd::AsFd::as_fd(input).try_clone_to_owned()?);
        std::thread::spawn(move || std::io::copy(&mut std::io::stdin(), &mut feed));
    }
    let status = vm.wait()?;
    drop(input);
    drop(daemons);
    drop(gpu);
    std::fs::remove_file(&client)?;
    guest_failed(&console)?;
    // QEMU's own last line, else its exit status
    let why = Some(last.join().unwrap_or_default()).filter(|l| !l.is_empty());
    let why = why.unwrap_or_else(|| status.to_string());
    anyhow::ensure!(status.success(), "the VM failed: {why}");
    Ok(())
}

/// Each line of `from` on this process's stderr; the last one that is not empty.
fn last_line(from: impl std::io::Read) -> String {
    let (mut from, mut last, mut line) = (std::io::BufReader::new(from), String::new(), Vec::new());
    // bytes, not text: a line that is not UTF-8 must not stop the relay (QEMU would block on a full pipe)
    while std::io::BufRead::read_until(&mut from, b'\n', &mut line).is_ok_and(|n| n > 0) {
        let _ = std::io::Write::write_all(&mut std::io::stderr(), &line);
        let text = String::from_utf8_lossy(&line);
        if !text.trim().is_empty() {
            last = text.trim().to_string();
        }
        line.clear();
    }
    last
}

/// The agent's channel on QEMU's stdio: a console for an interactive agent; for a headless one a port, and a port
/// for its stderr that is this process's stderr.
fn agent_channel(interactive: bool) -> Vec<String> {
    if interactive {
        return vec!["-device".into(), "virtconsole,chardev=agent".into()];
    }
    vec![
        "-device".into(),
        format!("virtserialport,chardev=agent,name={PORT}"),
        "-chardev".into(),
        "file,id=stderr,path=/proc/self/fd/2,append=on".into(),
        "-device".into(),
        format!("virtserialport,chardev=stderr,name={STDERR_PORT}"),
    ]
}

/// The guest's own error (its line in the VM's `console`), as the run's.
fn guest_failed(console: &Path) -> Result<()> {
    // the console has the boot's terminal codes too; none: QEMU did not start the VM (its own line says why)
    let log = match std::fs::read(console) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        read => read.with_context(|| format!("reading {}", console.display()))?,
    };
    match legionctl_core::session::guest_error(&String::from_utf8_lossy(&log)) {
        Some(why) => anyhow::bail!("the VM did not start the agent: {why}"),
        None => Ok(()),
    }
}

/// The NVIDIA GPU's IOMMU group: every device in it (the GPU and its audio function).
fn nvidia_group() -> Result<Vec<String>> {
    let pci = Path::new("/sys/bus/pci/devices");
    let read = |p: &Path| {
        std::fs::read_to_string(p)
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let gpu = std::fs::read_dir(pci)?
        .flatten()
        .map(|e| e.path())
        .find(|d| read(&d.join("vendor")) == "0x10de" && read(&d.join("class")).starts_with("0x03"))
        .context("no NVIDIA GPU on this machine")?;
    let group = std::fs::read_link(gpu.join("iommu_group"))
        .context("the NVIDIA GPU has no IOMMU group: turn on the IOMMU (intel_iommu=on)")?;
    let mut devices: Vec<String> = std::fs::read_dir(gpu.join("iommu_group/devices"))
        .with_context(|| format!("reading IOMMU group {}", group.display()))?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    devices.sort();
    Ok(devices)
}

/// Devices bound to vfio-pci for this VM; they go back to their drivers when it is dropped.
struct Passthrough {
    devices: Vec<String>,
    runtime: tokio::runtime::Runtime,
    client: legionctl_api::client::Client,
}

impl Passthrough {
    fn begin(devices: Vec<String>, memlock_mib: u64) -> Result<Passthrough> {
        let runtime = tokio::runtime::Runtime::new()?;
        let client = legionctl_api::client::Client::new("cli")?;
        runtime.block_on(client.call::<legionctl_api::op::VfioBind>(
            &legionctl_api::VfioBindIn {
                devices: devices.clone(),
                pid: Some(std::process::id()),
                memlock_mib: Some(memlock_mib),
            },
        ))?;
        Ok(Passthrough {
            devices,
            runtime,
            client,
        })
    }
}

impl Drop for Passthrough {
    fn drop(&mut self) {
        let back =
            self.runtime
                .block_on(self.client.call::<legionctl_api::op::VfioUnbind>(
                    &legionctl_api::VfioIn {
                        devices: self.devices.clone(),
                    },
                ));
        if let Err(e) = back {
            eprintln!(
                "legionctl vm: the GPU did not go back to its driver ({e:#}); run `legionctl vfio unbind {}`",
                self.devices.join(" ")
            );
        }
    }
}

/// QEMU's user network, restricted (no way to the host or out), with the guest's forwards only: the operator socket,
/// and the gateway socket when it serves.
fn netdev(me: &Path, spec: &VmSpec) -> String {
    let relay = |to: &str, socket: &Path| {
        format!(
            ",guestfwd=tcp:{to}-cmd:'{}' relay '{}'",
            me.display(),
            socket.display()
        )
    };
    let mut netdev = format!("user,id=net,restrict=on{}", relay(OPERATOR, &spec.socket));
    if spec.guest.gateway.is_some() {
        netdev.push_str(&relay(GATEWAY, &spec.socket.with_file_name(GATEWAY_SOCKET)));
    }
    netdev
}

/// A virtiofs daemon for each of `shares` (tag, folder, read-only), and its device on `qemu`.
pub(crate) fn share(
    qemu: &mut Command,
    folder: &Path,
    virtiofsd: &str,
    shares: &[&(String, std::path::PathBuf, bool)],
) -> Result<Daemons> {
    let mut daemons = Daemons(Vec::new());
    for (i, (tag, dir, read_only)) in shares.iter().map(|s| (&s.0, &s.1, s.2)).enumerate() {
        let socket = folder.join(format!("fs{i}.sock"));
        let log = folder.join(format!("fs{i}.log"));
        let _ = std::fs::remove_file(&socket);
        let mut daemon = Command::new(virtiofsd);
        daemon
            .arg(format!("--socket-path={}", socket.display()))
            .arg(format!("--shared-dir={}", dir.display()))
            .arg("--sandbox=namespace")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log)?);
        if read_only {
            daemon.arg("--readonly");
        }
        daemons.0.push(
            daemon
                .spawn()
                .with_context(|| format!("starting {virtiofsd}"))?,
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while !socket.exists() {
            let ended = daemons
                .0
                .last_mut()
                .map(Child::try_wait)
                .transpose()?
                .flatten();
            if ended.is_some() || Instant::now() > deadline {
                anyhow::bail!(
                    "{virtiofsd} did not share {}: {}",
                    dir.display(),
                    std::fs::read_to_string(&log).unwrap_or_default().trim()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        qemu.arg("-chardev")
            .arg(format!("socket,id=fs{i},path={}", socket.display()))
            .arg("-device")
            .arg(format!("vhost-user-fs-pci,chardev=fs{i},tag={tag}"));
    }
    Ok(daemons)
}

/// `legionctl relay <socket>`: one connection between this process's stdio and the operator socket.
///
/// # Errors
/// The socket refuses the connection, or a copy fails.
pub async fn relay(socket: &Path) -> Result<()> {
    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .with_context(|| format!("connecting to {}", socket.display()))?;
    let (mut from, mut to) = stream.into_split();
    let up = async {
        tokio::io::copy(&mut tokio::io::stdin(), &mut to).await?;
        to.shutdown().await
    };
    let down = async {
        let mut stdout = tokio::io::stdout();
        tokio::io::copy(&mut from, &mut stdout).await?;
        stdout.flush().await
    };
    tokio::try_join!(up, down)?;
    Ok(())
}

/// `legionctl forward <port> <socket> -- <agent>` (a session's container): while the agent runs, each connection to
/// 127.0.0.1:`port` goes to `socket` (the host's gateway, in the mounted runtime folder). Ends with the agent's status.
///
/// The connection of a relay's listener; a listener that fails ends the relay process, with the reason (the agent
/// behind it would see only resets).
fn accepted<S, A>(relay: &str, accepted: std::io::Result<(S, A)>) -> S {
    match accepted {
        Ok((stream, _)) => stream,
        Err(e) => {
            eprintln!("{relay}: accepting a connection: {e}");
            std::process::exit(1);
        }
    }
}

/// One relayed connection: `caller` both ways to `target`; a target that does not answer, or a copy that breaks, is
/// said on stderr with `what`.
async fn pipe<C, T>(what: &str, mut caller: C, target: std::io::Result<T>)
where
    C: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let copied = match target {
        Ok(mut target) => tokio::io::copy_bidirectional(&mut caller, &mut target)
            .await
            .map(|_| ()),
        Err(e) => Err(e),
    };
    if let Err(e) = copied {
        eprintln!("{what}: {e}");
    }
}

/// # Errors
/// The port is taken, or the agent cannot start.
pub fn forward(port: u16, socket: &Path, agent: &[String]) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind(("127.0.0.1", port)))
        .with_context(|| format!("port {port}"))?;
    let socket = socket.to_path_buf();
    runtime.spawn(async move {
        loop {
            let caller = accepted("forward", listener.accept().await);
            let socket = socket.clone();
            tokio::spawn(async move {
                let gateway = tokio::net::UnixStream::connect(&socket).await;
                pipe(&format!("forward: {}", socket.display()), caller, gateway).await;
            });
        }
    });
    let (program, args) = agent.split_first().context("no agent command")?;
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("running {program}"))?;
    std::process::exit(status.code().unwrap_or(1));
}

/// `from` to `to` until its end, with plain reads and writes: `std::io::copy` splices a pipe into a virtio port, which
/// takes nothing (it returns 0, and the agent then dies of SIGPIPE).
fn pass(from: &mut impl std::io::Read, to: &mut impl std::io::Write) -> std::io::Result<()> {
    let mut buffer = [0; 16384];
    loop {
        let n = match from.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        to.write_all(&buffer[..n])?;
        to.flush()?;
    }
}

/// `mount <args> <path>`; with `make`, a missing folder `path` is made first. A bind's mount point is never made: in a
/// writable share that would write on the host.
fn mount(args: &[&str], path: &Path, make: bool) -> Result<()> {
    if make && !path.exists() {
        std::fs::create_dir_all(path)
            .with_context(|| format!("making the mount point {}", path.display()))?;
    }
    let out = Command::new("mount")
        .args(args)
        .arg(path)
        .output()
        .context("running mount")?;
    anyhow::ensure!(
        out.status.success(),
        "mount {} {}: {}",
        args.join(" "),
        path.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

/// The guest's mounts of `spec`, in order.
fn mounts(spec: &GuestSpec) -> Result<()> {
    let owner = format!("uid={},gid={},mode=0700", spec.uid, spec.gid);
    for m in &spec.mounts {
        match m {
            GuestMount::Share {
                tag,
                path,
                read_only,
            } => {
                let options = if *read_only { "ro" } else { "rw" };
                mount(&["-t", "virtiofs", "-o", options, tag], path, true)?;
            }
            GuestMount::Empty(path) => mount(&["-t", "tmpfs", "-o", &owner, "tmpfs"], path, true)?,
            GuestMount::Bind {
                from,
                to,
                read_only,
            } => {
                mount(&["--bind", &from.to_string_lossy()], to, false)?;
                if *read_only {
                    mount(&["-o", "remount,bind,ro"], to, false)?;
                }
            }
            GuestMount::File(path, text) => {
                std::fs::write(path, text)
                    .with_context(|| format!("writing {}", path.display()))?;
                std::os::unix::fs::chown(path, Some(spec.uid), Some(spec.gid))
                    .with_context(|| format!("giving {} to the user", path.display()))?;
            }
        }
    }
    Ok(())
}

/// `legionctl guest`: the session in this VM (as root, at boot), then power off, also after an error (the unit's
/// output is the serial console, which the host keeps as `console.log`).
///
/// # Errors
/// The spec share, a mount, the console or the agent cannot be set up.
pub fn guest() -> Result<()> {
    let session = session();
    if let Err(e) = &session {
        eprintln!("legionctl guest: {e:#}");
    }
    Command::new("systemctl").arg("poweroff").status()?;
    session
}

fn session() -> Result<()> {
    let file = Path::new(SPEC_DIR).join("guest.json");
    let spec: GuestSpec = serde_json::from_str(
        &std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?,
    )
    .with_context(|| format!("reading {}", file.display()))?;
    mounts(&spec)?;
    // the operator socket at its usual path: each connection goes to the host
    let runtime = tokio::runtime::Runtime::new().context("starting the relays")?;
    let socket = spec.socket.display();
    let listener = runtime
        .block_on(async { tokio::net::UnixListener::bind(&spec.socket) })
        .with_context(|| format!("listening at {socket}"))?;
    std::os::unix::fs::chown(&spec.socket, Some(spec.uid), Some(spec.gid))
        .with_context(|| format!("giving {socket} to the user"))?;
    runtime.spawn(async move {
        loop {
            let agent = accepted("relay", listener.accept().await);
            tokio::spawn(async move {
                let host = tokio::net::TcpStream::connect(OPERATOR).await;
                pipe(&format!("relay: the operator at {OPERATOR}"), agent, host).await;
            });
        }
    });
    if let Some(port) = spec.gateway {
        // the gateway at 127.0.0.1:<port>, as on the host: each connection goes to the host's gateway socket
        let gateway = runtime
            .block_on(tokio::net::TcpListener::bind(("127.0.0.1", port)))
            .with_context(|| format!("listening on 127.0.0.1:{port}"))?;
        runtime.spawn(async move {
            loop {
                let agent = accepted("relay", gateway.accept().await);
                tokio::spawn(async move {
                    let host = tokio::net::TcpStream::connect(GATEWAY).await;
                    pipe(&format!("relay: the gateway at {GATEWAY}"), agent, host).await;
                });
            }
        });
    }
    let channel = if spec.interactive {
        CONSOLE.to_string()
    } else {
        format!("/dev/virtio-ports/{PORT}")
    };
    let console = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&channel)
        .with_context(|| format!("opening {channel}"))?;
    let (uid, gid) = (spec.uid.to_string(), spec.gid.to_string());
    // as the user, without root's groups; an interactive agent gets the console as its terminal
    let mut agent = Command::new(if spec.interactive {
        "setsid"
    } else {
        "setpriv"
    });
    if spec.interactive {
        agent.args(["--ctty", "setpriv"]);
    }
    agent
        .args(["--reuid", &uid, "--regid", &gid, "--clear-groups", "--"])
        .args(&spec.argv)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&spec.workdir);
    if spec.interactive {
        agent
            .stdin(
                console
                    .try_clone()
                    .with_context(|| format!("opening {channel}"))?,
            )
            .stdout(
                console
                    .try_clone()
                    .with_context(|| format!("opening {channel}"))?,
            )
            .stderr(console);
        agent.status().context("running the agent")?;
        return Ok(());
    }
    headless(agent, console, &channel)
}

/// A headless agent with pipes, as on the host: a runtime that reads its input as a file (bun: pread) fails on a
/// port; its stderr goes apart from its output (the ACP channel), to the host's stderr.
fn headless(mut agent: Command, console: std::fs::File, channel: &str) -> Result<()> {
    let port = format!("/dev/virtio-ports/{STDERR_PORT}");
    let mut stderr = std::fs::OpenOptions::new()
        .write(true)
        .open(&port)
        .with_context(|| format!("opening {port}"))?;
    let mut child = agent
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running the agent")?;
    let (mut input, mut output, mut errors) = (
        child.stdin.take().context("the agent's stdin")?,
        child.stdout.take().context("the agent's stdout")?,
        child.stderr.take().context("the agent's stderr")?,
    );
    let mut from = console
        .try_clone()
        .with_context(|| format!("opening {channel}"))?;
    std::thread::spawn(move || pass(&mut from, &mut input));
    let mut to = console;
    let (done, copied) = std::sync::mpsc::channel();
    let also = done.clone();
    std::thread::spawn(move || done.send(pass(&mut output, &mut to)));
    std::thread::spawn(move || also.send(pass(&mut errors, &mut stderr)));
    child.wait().context("waiting for the agent")?;
    // its output and errors reach the host before the VM powers off; a process it left behind with them open does
    // not hold the VM
    let deadline = Instant::now() + Duration::from_secs(5);
    for _ in 0..2 {
        let left = deadline.saturating_duration_since(Instant::now());
        if copied.recv_timeout(left).is_err() {
            break;
        }
    }
    Ok(())
}
