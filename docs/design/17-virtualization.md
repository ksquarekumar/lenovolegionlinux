# 17 — Virtualization, containers and emulation

Requirements (2026-10-12): the operator and the daemon list, create and tune VMs and containers, with compute and
graphics offload to the GPU; they check that the virtualization, container and emulation stacks are up and healthy,
and warn when they are not; they know which apps (Docker, WinBoat, Steam, ...) provide or consume which of these
capabilities on this OS and hardware.

## Capability model

Capabilities are what this OS and hardware offer. Providers implement them. Consumers use them. legionctl keeps all
three in one inventory, so a health problem in a provider shows which consumers it affects.

| Capability | Provider on this machine (measured 2026-10-12) |
| --- | --- |
| Hardware virtualization | KVM (`/dev/kvm`, `kvm_intel`), VT-d/IOMMU, VFIO (`/dev/vfio`) |
| VMs | QEMU (`qemu-full`), libvirt (monolithic `libvirtd` active; modular `virtqemud` inactive), OVMF (`edk2-ovmf`), swtpm, virtiofsd |
| Containers | Docker 29.9 on containerd, overlay2, data root `/data/builds/docker`; Flatpak; bwrap (legionctl sessions, Steam pressure-vessel) |
| GPU compute in containers | NVIDIA container runtime (`nvidia-ctk`, runtime `nvidia` in `daemon.json`) |
| GPU graphics in VMs | virtio-gpu GL and Venus (`qemu-hw-display-virtio-gpu-*-gl`, rutabaga), VirtualGL |
| GPU offload on the host | NVIDIA PRIME render offload; `legionctl gpu run` (02-platform) |
| Foreign-architecture emulation | `qemu-user-static` with binfmt (aarch64, arm, riscv, ...) |
| Windows binaries | Wine (`wine-cachyos`, binfmt `DOSWin`), Proton (`proton-cachyos-slr`, `umu-launcher`), Mono binfmt `CLR` |

| Consumer | Uses |
| --- | --- |
| WinBoat | Docker container `ghcr.io/dockur/windows` with KVM inside, FreeRDP for the desktop (running 5 h, 16 GiB of 31 GiB RAM) |
| Steam, Lutris | Proton/Wine, pressure-vessel containers, NVIDIA offload |
| Docker workloads | containers, NVIDIA runtime for CUDA, binfmt for multi-arch builds |
| Installed-Windows VM | libvirt + KVM + VFIO passthrough of the Windows NVMe (05-windows) |
| legionctl sessions | bwrap sandboxes (14-sessions) |

## Operations

`legionctl virt` (operations in the registry, 15-layout):

| Operation | Does |
| --- | --- |
| `virt status` | each capability: provider, version, up/down, health checks, consumers |
| `virt list` | one list of VMs (libvirt), containers (Docker) and known consumer instances (WinBoat is recognized by its image), with CPU, memory, GPU use and uptime |
| `virt create` | from a template: VM (libvirt domain) or container; options for CPU set, memory, disk on `/data`, GPU offload (below) |
| `virt start/stop/pause` | for any listed instance; pause frees CPU at once (container freeze, VM pause) |
| `virt tune` | CPU pinning to P-cores or E-cores (02-platform core classes), memory limit or balloon, hugepages, I/O on `/data`; previewed as a plan |

One API per provider: the Docker Engine API on its socket and the libvirt API (`qemu:///system`). No CLI parsing.

Built (2026-10-13, V1 part 1): `legionctl virt status`: KVM (`/dev/kvm` and its module), IOMMU, VFIO and binfmt
from the kernel; Docker through its Engine API on its socket (`DOCKER_HOST` when set): version, data root, the
NVIDIA runtime, the cgroup driver, and the containers (`WinBoat` recognized by its image); libvirt: `libvirtd` or the
modular `virtqemud` (service or socket active: the modular daemons exit when idle), and its VMs through `virsh
--readonly list --all` (libvirt has no socket API without its C library; this is the one CLI read). Findings: no
KVM, a cgroupfs driver on a systemd host, both libvirt daemons, more than one docker CLI on PATH; findings fail the
command. Not in `doctor` yet (they depend on the host's Docker and would make the doctor test differ between
machines). Live on this machine: up everywhere but the cgroupfs finding; WinBoat found.

Built (2026-10-13, V1 part 2): `legionctl virt act pause|unpause|start|stop <name>`: a container through
the Engine API (`POST /containers/<name>/<action>`), a VM through `virsh` (suspend, resume, start, shutdown); a name
that is neither is refused. Checked live on a throwaway container (paused, running, exited).

Built (2026-10-13, V1 part 3): memory per running instance in `virt status` (`memory_bytes`): a container's use
without its inactive page cache, as `docker stats` counts it (Engine API `stats?stream=false&one-shot=true`), and a
VM's resident memory (`virsh --readonly domstats --balloon --list-running`: `balloon.rss`, else the balloon size).
When available memory is below `load.busy.memory_available_percent` (the same limit as the load module), a finding
names what the guests hold, largest first, and how to pause one. CPU use per instance is not shown: it needs two
samples per container (about 2 s per call). The live check found the wrong `domstats` option
(`--state-running`); the stand-in `virsh` now refuses options it does not expect.

Built (2026-10-13, V1 part 4): `legionctl virt tune <name> [--cores p|e|all] [--memory <size>] [--dry-run]`: the
plan (the CPU list from `/sys/devices/cpu_core/cpus`, `cpu_atom/cpus` or `/sys/devices/system/cpu/online`; the size
in bytes, with k, m or g as powers of 1024), then the change unless dry run. A container: Engine API
`POST /containers/<name>/update` with `CpusetCpus`, and `Memory` with `MemorySwap` set to the same value (no swap
above the limit; with `Memory` alone Docker keeps the old swap limit and refuses a raise above it). A VM: `virsh vcpupin --vcpu <n> --cpulist <cpus> --live` for each live vCPU
(`vcpucount --active --live`) and `virsh setmem <KiB> --live` (the balloon). A container's update stays with the
container; a VM's pinning and balloon are live only, so its own settings return at its next start. Not live-checked on WinBoat (the user's VM stays as it is); the e2e test uses
the stand-in Docker and virsh, with the command forms checked against `virsh --help`.

Built (2026-10-13, V1 part 5): `legionctl virt create <name> <image> [--cores p|e|all] [--memory <size>] [--gpu]
[--dry-run]`: a container from a local image (no pull) through the Engine API (`POST /containers/create`: `Image`,
`HostConfig.CpusetCpus`, `Memory`, and for `--gpu` a `DeviceRequests` entry for all NVIDIA GPUs), created and not
started (`virt act start <name>`). A taken name, and `--gpu` without Docker's nvidia runtime, are refused before
anything is created. Checked live: the same request makes a container with the CPU set and limit, in state
`created` (removed again). VM create waits for W5 (base images, decision D5).

### GPU offload options

| Target | Compute | Graphics |
| --- | --- | --- |
| Container | NVIDIA runtime (CDI devices) | `/dev/dri` render node of the chosen GPU; PRIME offload variables for NVIDIA |
| VM | none without passthrough | virtio-gpu Venus (Vulkan) or GL on the host GPU |
| VM with passthrough | the dGPU | the dGPU; only possible while no host output is on NVIDIA (the external display is wired to NVIDIA), so it needs a planned mode change and a new login |

## Health checks (doctor group "virtualization")

| Check | Found on this machine |
| --- | --- |
| One CLI per tool | `/usr/local/bin/docker` (no package owns it) shadows the packaged `/usr/bin/docker` (both 29.9.0) |
| Docker cgroup driver | `native.cgroupdriver=cgroupfs` on a systemd, cgroup v2 host; the systemd driver is the supported setup |
| Docker disk use | 32 GB of 41 GB images reclaimable, 50 GB build cache on `/data` |
| Memory held by guests | WinBoat holds 16 GiB of 31 GiB; warn when host free memory is low and offer pause or stop |
| NVIDIA runtime | CDI spec and container toolkit match the installed driver (regenerate after each driver update) |
| KVM and IOMMU | `kvm_intel` loaded, IOMMU on, VFIO binding only while the Windows VM runs (05-windows) |
| libvirt daemons | monolithic `libvirtd` or the modular daemons, not both |
| binfmt | expected registrations present (qemu-user, Wine, Mono) |
| Steam and Proton | 32-bit NVIDIA libraries match the driver version; shader cache size |

Warnings go to the event log, `legionctl doctor`, the app and the OSD. Each has a fix operation with a preview.

## Ownership and security

- `legionctl-system` owns the kernel parts: KVM and VFIO modules, VFIO device binding, hugepages, binfmt
  registrations, cgroup settings.
- `legionctl-operator` owns the user-level parts: Docker and libvirt instances, templates, consumer inventory.
- Docker socket access is root-equivalent (this user is in the `docker` group). Docker operations that mount host
  paths or add devices are machine changes and need approval (12-operator approvals).
