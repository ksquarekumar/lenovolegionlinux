//! Sessions in a VM (W5 part 3, note 18). On the host, `legionctl vm` runs a session's VM inside the session's
//! scope: a virtiofs daemon per shared folder, then QEMU on a throwaway layer over the repository's disk, with the
//! agent on the virtio console (this process's stdio); each guest connection to the operator address goes through
//! `legionctl relay` to the operator socket. In the VM, `legionctl guest` (the static client, started at boot)
//! mounts the shares, gives the agent the operator socket at its usual path, runs the agent as the user on the
//! console, and powers the VM off when the agent ends.

use std::io::IsTerminal;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use legionctl_core::session::{GuestMount, GuestSpec, VmSpec};
use tokio::io::AsyncWriteExt;

/// The operator as the guest reaches it: QEMU's user network runs `legionctl relay` for each connection.
const OPERATOR: &str = "10.0.2.100:80";
/// The share with `guest.json`, and where the guest mounts it.
const SPEC_TAG: &str = "legionctl-spec";
const SPEC_DIR: &str = "/run/legionctl-spec";
/// The agent's channel in the guest: a terminal for an interactive agent; for a headless one a port without a line
/// discipline, which would echo the client's first request back (QEMU holds the input until the guest opens it).
const CONSOLE: &str = "/dev/hvc0";
const PORT: &str = "org.legionctl.agent";

/// The virtiofs daemons of a VM; they go when the VM ends.
struct Daemons(Vec<Child>);

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
    .arg("-device")
    .arg(if spec.guest.interactive {
        "virtconsole,chardev=agent".to_string()
    } else {
        format!("virtserialport,chardev=agent,name={PORT}")
    })
    .arg("-netdev")
    .arg(format!(
        "user,id=net,guestfwd=tcp:{OPERATOR}-cmd:'{}' relay '{}'",
        me.display(),
        spec.socket.display()
    ))
    .args(["-device", "virtio-net-pci,netdev=net"]);
    let spec_share = (SPEC_TAG.to_string(), guest_dir, true);
    let shares: Vec<_> = spec.shares.iter().chain([&spec_share]).collect();
    let daemons = share(&mut qemu, folder, &spec.virtiofsd, &shares)?;
    let status = if spec.guest.interactive {
        qemu.status().context("running qemu-system-x86_64")?
    } else {
        // QEMU closes the guest's channel at the end of its stdin, and the agent's writes then block: feed it
        // through a pipe that stays open until the VM ends
        let mut vm = qemu
            .stdin(Stdio::piped())
            .spawn()
            .context("running qemu-system-x86_64")?;
        let input = vm.stdin.take().context("QEMU's stdin")?;
        let mut feed = std::fs::File::from(std::os::fd::AsFd::as_fd(&input).try_clone_to_owned()?);
        std::thread::spawn(move || std::io::copy(&mut std::io::stdin(), &mut feed));
        let status = vm.wait()?;
        drop(input);
        status
    };
    drop(daemons);
    anyhow::ensure!(
        status.success(),
        "the VM failed ({status}); its console is in {}",
        console.display()
    );
    Ok(())
}

/// A virtiofs daemon for each of `shares` (tag, folder, read-only), and its device on `qemu`.
fn share(
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

fn mount(args: &[&str], path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    let out = Command::new("mount").args(args).arg(path).output()?;
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
                mount(&["-t", "virtiofs", "-o", options, tag], path)?;
            }
            GuestMount::Empty(path) => mount(&["-t", "tmpfs", "-o", &owner, "tmpfs"], path)?,
            GuestMount::File(path, text) => {
                std::fs::write(path, text)?;
                std::os::unix::fs::chown(path, Some(spec.uid), Some(spec.gid))?;
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
    mount(
        &["-t", "virtiofs", "-o", "ro", SPEC_TAG],
        Path::new(SPEC_DIR),
    )?;
    let spec: GuestSpec = serde_json::from_str(&std::fs::read_to_string(
        Path::new(SPEC_DIR).join("guest.json"),
    )?)?;
    mounts(&spec)?;
    // the operator socket at its usual path: each connection goes to the host
    let runtime = tokio::runtime::Runtime::new()?;
    let listener = runtime.block_on(async { tokio::net::UnixListener::bind(&spec.socket) })?;
    std::os::unix::fs::chown(&spec.socket, Some(spec.uid), Some(spec.gid))?;
    runtime.spawn(async move {
        while let Ok((mut agent, _)) = listener.accept().await {
            tokio::spawn(async move {
                if let Ok(mut host) = tokio::net::TcpStream::connect(OPERATOR).await {
                    let _ = tokio::io::copy_bidirectional(&mut agent, &mut host).await;
                }
            });
        }
    });
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
        .current_dir(&spec.workdir)
        .stdin(console.try_clone()?)
        .stdout(console.try_clone()?)
        .stderr(console);
    agent.status().context("running the agent")?;
    Ok(())
}
