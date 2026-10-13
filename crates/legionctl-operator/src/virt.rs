//! Virtualization, containers and emulation (note 17, V1 part 1): what this machine offers, which provider gives it,
//! whether it is up, what is wrong with it, and the running instances. Docker through its Engine API on its socket
//! (`DOCKER_HOST` when set); libvirt through `virsh --readonly`, as libvirt has no socket API without its C library.

use std::path::PathBuf;

use anyhow::{Context, Result};
use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use legionctl_api::{VirtCapability, VirtInstance, VirtStatus};
use legionctl_machine::sysroot::SysRoot;
use serde_json::Value;

fn docker_socket() -> PathBuf {
    std::env::var("DOCKER_HOST")
        .ok()
        .and_then(|h| h.strip_prefix("unix://").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/var/run/docker.sock"))
}

/// One GET on the Docker Engine API.
async fn docker(path: &str) -> Result<Value> {
    let socket = docker_socket();
    let stream = tokio::net::UnixStream::connect(&socket)
        .await
        .with_context(|| format!("Docker does not answer on {}", socket.display()))?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream)).await?;
    tokio::spawn(connection);
    let request = hyper::Request::get(path)
        .header("host", "docker")
        .body(Empty::<Bytes>::new())?;
    let response = sender.send_request(request).await?;
    anyhow::ensure!(
        response.status().is_success(),
        "Docker {path}: {}",
        response.status()
    );
    Ok(serde_json::from_slice(
        &response.into_body().collect().await?.to_bytes(),
    )?)
}

fn capability(name: &str, provider: &str, up: bool, detail: String) -> VirtCapability {
    VirtCapability {
        name: name.into(),
        provider: provider.into(),
        up,
        detail,
    }
}

/// Every `docker` on `PATH`; more than one means a stray copy shadows the packaged one.
fn docker_clis() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("docker"))
                .filter(|p| p.is_file())
                .collect()
        })
        .unwrap_or_default();
    // /usr/sbin and /usr/bin are one folder on merged-/usr systems
    found.dedup_by(|a, b| std::fs::canonicalize(&*a).ok() == std::fs::canonicalize(&*b).ok());
    found
}

fn libvirt_vms() -> Vec<VirtInstance> {
    let Ok(out) = std::process::Command::new("virsh")
        .args(["-c", "qemu:///system", "--readonly", "list", "--all"])
        .output()
    else {
        return Vec::new();
    };
    // " Id   Name   State" then a rule, then "-    win11   shut off"
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(2)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let (_, name) = (parts.next()?, parts.next()?);
            Some(VirtInstance {
                kind: "vm".into(),
                name: name.into(),
                image: String::new(),
                state: parts.collect::<Vec<_>>().join(" "),
                consumer: None,
            })
        })
        .collect()
}

/// KVM, IOMMU, VFIO and binfmt from the kernel.
fn kernel(root: &SysRoot) -> Vec<VirtCapability> {
    let modules = legionctl_machine::kernel::loaded_modules(root);
    let kvm =
        root.exists("/dev/kvm") && (modules.contains("kvm_intel") || modules.contains("kvm_amd"));
    let iommu = !root.children("/sys/class/iommu", "").is_empty();
    let binfmt: Vec<String> = root
        .children("/proc/sys/fs/binfmt_misc", "")
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .filter(|n| n != "register" && n != "status")
        .collect();
    vec![
        capability(
            "Hardware virtualization",
            "KVM",
            kvm,
            if kvm {
                "/dev/kvm".into()
            } else {
                "no /dev/kvm or KVM module".into()
            },
        ),
        capability(
            "IOMMU",
            "VT-d",
            iommu,
            if iommu {
                "on".into()
            } else {
                "off: no /sys/class/iommu entry".into()
            },
        ),
        capability(
            "Device passthrough",
            "VFIO",
            root.exists("/dev/vfio"),
            "/dev/vfio".into(),
        ),
        capability(
            "Foreign binaries",
            "binfmt_misc",
            !binfmt.is_empty(),
            binfmt.join(", "),
        ),
    ]
}

/// Docker's capabilities, findings and containers.
async fn containers(root: &SysRoot, status: &mut VirtStatus) {
    let (version, info) = match (docker("/version").await, docker("/info").await) {
        (Ok(v), Ok(i)) => (v, i),
        (Err(e), _) | (_, Err(e)) => {
            status
                .capabilities
                .push(capability("Containers", "Docker", false, format!("{e:#}")));
            return;
        }
    };
    let root_dir = info["DockerRootDir"].as_str().unwrap_or("?");
    status.capabilities.push(capability(
        "Containers",
        "Docker",
        true,
        format!("{}, {root_dir}", version["Version"].as_str().unwrap_or("?")),
    ));
    let nvidia = info["Runtimes"].get("nvidia").is_some();
    let runtime = if nvidia {
        "runtime nvidia"
    } else {
        "no nvidia runtime"
    };
    status.capabilities.push(capability(
        "GPU compute in containers",
        "NVIDIA runtime",
        nvidia,
        runtime.into(),
    ));
    if info["CgroupDriver"] != "systemd" && root.exists("/run/systemd/system") {
        status.findings.push(format!(
            "Docker uses the {} cgroup driver on a systemd host; the systemd driver is the supported setup",
            info["CgroupDriver"].as_str().unwrap_or("?")
        ));
    }
    let list = docker("/containers/json?all=1")
        .await
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    for c in list {
        let image = c["Image"].as_str().unwrap_or("").to_string();
        status.instances.push(VirtInstance {
            kind: "container".into(),
            name: c["Names"][0]
                .as_str()
                .unwrap_or("?")
                .trim_start_matches('/')
                .into(),
            consumer: image
                .starts_with("ghcr.io/dockur/windows")
                .then(|| "WinBoat".into()),
            image,
            state: c["Status"].as_str().unwrap_or("").into(),
        });
    }
}

/// libvirt's daemons and VMs.
fn vms(status: &mut VirtStatus) {
    // socket-activated daemons exit when idle: the socket being active counts as up
    let up = |name: &str| {
        ["service", "socket"].iter().any(|kind| {
            legionctl_machine::system::unit_state("is-active", &format!("{name}.{kind}"))
                == "active"
        })
    };
    let (monolithic, modular) = (up("libvirtd"), up("virtqemud"));
    let daemons = match (monolithic, modular) {
        (true, false) => "libvirtd",
        (false, true) => "virtqemud",
        (true, true) => "libvirtd and virtqemud",
        (false, false) => "no libvirt daemon",
    };
    status.capabilities.push(capability(
        "VMs",
        "libvirt",
        monolithic || modular,
        daemons.into(),
    ));
    if monolithic && modular {
        status
            .findings
            .push("libvirtd and the modular virtqemud both run; use one of them".into());
    }
    status.instances.extend(libvirt_vms());
}

/// The status of every capability, the instances, and what is wrong.
pub async fn status(root: &SysRoot) -> VirtStatus {
    let mut status = VirtStatus {
        capabilities: kernel(root),
        instances: Vec::new(),
        findings: Vec::new(),
    };
    if !status.capabilities[0].up {
        status
            .findings
            .push("no KVM: VMs run without hardware virtualization (slow)".into());
    }
    containers(root, &mut status).await;
    let clis = docker_clis();
    if clis.len() > 1 {
        let list: Vec<String> = clis.iter().map(|p| p.display().to_string()).collect();
        status.findings.push(format!(
            "more than one docker CLI on PATH; the first shadows the others: {}",
            list.join(", ")
        ));
    }
    vms(&mut status);
    status
}
