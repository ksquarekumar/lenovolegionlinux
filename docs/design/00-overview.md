# 00 — Overview

## Why redesign

The Python tooling grew by accretion: a oneshot `legionctl` run by udev, a 30 s reassert timer, a `follow`
service for Fn+Q, a user-session `legionctl-osd`, a refresh toggle, a sweep script. Each fixes one thing and they
coordinate through files in `/run` and timing assumptions. Gaps found on 2026-10-12 all come from that shape:

- nothing runs after suspend/resume, so firmware resets survive until the next AC change;
- reassert restores the firmware mode, governor and PPD but not power limits;
- two power-limit control surfaces (RAPL MSR/MMIO and Lenovo firmware attributes) are both live and disagree;
- every new trigger (key, uevent, D-Bus signal) needs another unit, rule or polling loop.

One long-running daemon that owns desired state and reacts to events removes the timer, the races and the
file-based coordination.

## Goals

1. **Event-driven**: react to udev, sysfs notify, logind, D-Bus and input events; no fixed-interval reassert.
   Knobs that cannot notify (sysctl, governor) are read by a periodic drift check; that is their only path.
2. **Declarative**: desired state comes from layered *configsets* (TOML). The daemon reconciles observed state
   toward it, including sysctl and every other knob.
3. **Modular**: sensors and actuators are modules behind traits, one path for each capability (15-layout).
4. **Safe**: writes are bounded, verified and rate-limited; a failed path puts the daemon in safe mode and reports
   why; the machine returns to firmware defaults if the daemon dies.
5. **Low-level where it pays**: eBPF for observation (workload detection, latency, power attribution), HID-BPF for
   input fixes, sched_ext through scx_loader. No eBPF for actuation.
6. **First-class session UX**: animated GNOME OSD, the GPUI app, Fn keys handled by the daemon.
7. **Dual boot aware**: detect and fix Windows-side hazards, reboot into Windows, and run the installed Windows
   under KVM when wanted.

Non-goals: generic Legion support (stays machine-specific per `profiles/`), fan *writes* before fan reads are
validated, replacing the kernel's in-tree Lenovo drivers.

## Measured constraints (2026-10-12)

| Fact | Consequence |
| --- | --- |
| `lenovo-wmi-other` firmware attributes: `ppt_pl1_spl` 90 W (30–140), `ppt_pl2_sppt` 135 (55–190), `ppt_cpu_cl` 75 (20–110), `cpu_temp` 100 (85–100), `gpu_temp` 87 (75–87), `gpu_nv_ac_offset` 75, `gpu_nv_ctgp` 150, `gpu_nv_ppab` 25 | The firmware's own limits (what Vantage sets on Windows) are writable from Linux. They should replace RAPL writes as the primary power-limit actuator. `ppt_cpu_cl` is the CPU limit under combined CPU+GPU load. |
| legionctl writes RAPL PL1 = 70 W while firmware SPL = 90 W; firmware mode changes rewrite the MMIO RAPL limit | Two surfaces fight; RAPL becomes a fallback only. |
| Fan RPM comes from `thinkpad_acpi` (`speed: 4`, `status: disabled`) | Bogus source on a Legion; needs a real fan sensor (see 02-platform). |
| eDP-1 on i915 (card1, boot VGA); external DP-3 on NVIDIA (card0); Mutter: "GPU card1 selected primary from builtin panel presence" | GNOME composites on Intel and copies to NVIDIA for the 144 Hz ultrawide: the likely cause of choppy animations. Primary GPU must follow where the active outputs are. |
| `org.gnome.Shell.ShowOSD` → AccessDenied; GNOME has no layer-shell | Native OSD needs a GNOME Shell extension; a GPUI overlay window is not possible on GNOME. |
| Fn+R emits a burst of ~10 key events per press; Fn+Q changes `platform_profile` in firmware and emits a uevent; Fn+Space/Fn+Esc are firmware-only (WMI 0xd0, `fn_lock` attribute) | Input handling needs debounce; firmware-initiated changes must be distinguishable from ours (ownership ledger). |
| Kernel has BTF, BPF JIT, BPF LSM, HID-BPF, sched_ext; scx_lavd running via scx_loader | eBPF plugins are viable without extra kernel work. |
| Windows on its own NVMe (`nvme0`, SK hynix P41), alone in IOMMU group 18, not BitLocker; ACPI `MSDM` present; Secure Boot on, TPM2 present | Running the installed Windows under KVM with the whole NVMe passed through is feasible, with activation preserved. |
| 33,251 package thermal-throttle events since boot at moderate temperatures | Throttle reasons must be observable (MSR status bits), not just counted. |

## Architecture

```mermaid
flowchart LR
  subgraph sources[Sensors / event sources]
    udev[udev: power_supply, platform-profile, drm, input]
    sysfs[sysfs notify; drift check for knobs without notify]
    logind[logind: PrepareForSleep, sessions]
    dbus[D-Bus: PPD, UPower, scx_loader]
    evdev[evdev: Fn keys]
    bpf[eBPF: exec, sched, power tracepoints]
    hw[hwmon / NVML / MSR / EC read-only]
  end
  subgraph legionctl-system[legionctl-system (root)]
    bus((event bus))
    policy[policy: active selectors]
    cfg[configset engine: layered merge + validation]
    rec[reconciler + ownership ledger]
    api[D-Bus API org.legionctl.System1 + polkit]
  end
  subgraph actuators[Actuators]
    fw[firmware attributes / platform_profile]
    cpu[cpufreq / EPP / sysctl / sysfs]
    ppd[PPD + scx_loader]
    nv[NVIDIA]
  end
  sources --> bus --> policy --> cfg --> rec --> actuators
  rec --> api
  api <--> op[legionctl-operator (user)]
  op --> osd[OSD: GNOME Shell extension]
  op --> mutter[Mutter DisplayConfig: refresh, primary GPU hints]
  op <--> clients[legionctl CLI / TUI / MCP, legionctl-app (GPUI)]
```

## Crates, processes and names

See 15-layout.

## Key dependencies (to confirm versions when Phase 1 starts)

`tokio`, `zbus` (D-Bus), `udev` (or `tokio-udev`), `evdev`, `serde` + `toml`, `nvml-wrapper`,
`aya`, `sd-notify`, `tracing` + `tracing-journald`, `gpui` (pinned by git rev).
