---
name: machine-tools
description: How to read and change this Lenovo Legion Pro 7 16IRX8H with legionctl - status, doctor, monitor, knobs and configset rules, power, fan, battery, display, GPU, kernel, boot, greeter, Windows, virtualization, bench, tune, record, replay and undo - which commands only read, which change the machine through the root daemon and polkit, and the safety rules for agents. Use before any legionctl machine command and whenever the user asks about power, heat, fans, GPU, display, kernels or boot.
user-invocable: true
---

# Machine tools

## How a machine command runs

1. The `legionctl` client sends the command to the operator (`legionctl-operator`, your user service).
2. A read runs in the operator as you. It changes nothing. Some values (fan RPM, throttle reasons) stay `unknown`
   without root.
3. A change (permission class `machine`) goes over D-Bus to the root daemon (`legionctl-system`). polkit shows a
   dialog on this machine, and the human approves or refuses it. The agent cannot approve it.
4. The operator plans first. A refusal (a wrong value, a missing boot takeover) comes before the dialog, with
   nothing changed. `--dry-run`, where the command has it, shows the plan and changes nothing.
5. The daemon writes, reads back, and records an event with the old values. `undo <event>` turns it back.

A `workspace`-class command (your own settings, containers, benches) runs as you. When an agent calls it, the user
gets a desktop notification with Allow and Deny.

## Safety rules for agents (from the user)

- "do not guess ACPI/WMI write calls on the real firmware"
- "never run `windows reboot`, tune sweeps or polkit writes live"
- "do not pause the user's WinBoat"
- "never return secrets"
- "safe live writes only (read-only checks, recorded runs, containers without GPU, short stress runs within
  firmware limits)"

So: read freely. For a change, give the user the exact command and the `--dry-run` output, and let the user run
it. Test changes against a recording (`record`, `replay`) or in the e2e tests, not on the live firmware.

## Read-only tools

| Command | What it shows |
| --- | --- |
| `status` | Power source, thermal mode (and the Fn+Q choice), firmware limits, CPU policy, temperature, GPU wiring, load. `--fast` reads sysfs and procfs only. |
| `load` | Load level (idle, normal, busy, overloaded) and why. Check it before builds and test suites. |
| `doctor` | Health findings: policy drift, conflicting units, packages, modules, GPU wiring, display, layers that fail their checks. Each graphics finding has an id in brackets. |
| `monitor` | Telemetry, one line per sample. `--count <n>` (1-3600), `--interval-ms <ms>` (100-60000). |
| `capabilities`, `validate` | Capability coverage from the profile; a check of the profile and the shipped layers (no hardware access). |
| `knobs list`, `knobs show <knob>` | Each knob: value now, the value the layers want, the firmware range, the policy; one knob's layer chain and history. |
| `plan`, `rules list`, `rules show <layer>` | The desired state now: conditions, the layers that apply, the writes the daemon would make. |
| `fan status`, `battery status` | Fan table, curve, fan max mode; battery mode, health, cycles, health trend. |
| `display status` | Monitors (mode, highest rate, scale, VRR, HDR), font antialiasing, findings with ids. |
| `gpu status`, `vfio status` | Which GPU drives each display and draws the desktop; which devices can go to a VM. |
| `kernel list`, `boot status` | Installed kernels (scheduler, LTO, image, command line, DKMS state, running, default); firmware boot entries. |
| `greeter status`, `windows status`, `windows vm status` | Login screen; dual boot and dirty NTFS volumes; the Windows VM. |
| `virt status` | Containers and VMs (WinBoat recognized) and what is wrong. |
| `bench list`, `bench compare <earlier> <later>`, `replay <dir>` | Bench runs; two runs side by side; what the daemon would write on a recorded machine. |

## Changes

| Command | What it changes | When it applies |
| --- | --- | --- |
| `power apply [--source auto\|ac\|battery] [--dry-run]` | The power policy. It clears an Fn+Q choice. | Now |
| `knobs hold <knob> <value>`, `knobs release <knob>` | Holds a knob against the policy; gives it back to the policy. | Now, until release |
| `rules edit <layer> [knob=value]... [--when <conditions>] [--system]` | Your configset layer (or a system layer). `knob=` removes a knob. Conditions: `power`, `fnq`, `workload`, `kernel`, or `always`. | Now (the layers apply) |
| `rules enable\|disable <layer>`, `rules reset [--knob <knob>] [--system]` | A layer on or off (`rules enable tuned` uses the `tune` result); back to the shipped state. | Now |
| `fan curve set <10 levels> [--dry-run]`, `fan curve reset`, `fan max on\|off`, `fan clean on\|off` | Fan curve (custom mode only), fan max mode, dust-clean speed. | Now; set again after resume |
| `fan lock on\|off` | A raw EC write. Refused until the profile marks it verified. | Do not use |
| `battery mode standard\|conservation\|rapid`, `battery usb-charging on\|off` | Charge mode; USB power while off. Both have `--dry-run`. | Now |
| `gpu primary nvidia\|intel\|auto [--release-nvidia] [--dry-run]` | The GPU that draws the desktop. | Next login |
| `gpu give igpu\|nvidia linux\|windows\|none`, `vfio bind\|unbind <devices>` | A GPU or an IOMMU group to Linux, to the Windows VM, or to no one. Apps on that GPU lose it. | Now |
| `display fix <id>\|all [--persistent] [--dry-run]` | Your own settings only: antialiasing, an app's flags file, a monitor's rate, the session environment. No daemon, no polkit (class `workspace`). | Now; a mode until next login without `--persistent` |
| `kernel default <name>`, `kernel cmdline set <name> "<line>"`, `kernel cmdline undo <name>` | The default kernel; one kernel's command line (its image only is made again and its signature checked). Needs `boot install` first. | Next start |
| `kernel try <name>` | Restarts into the kernel once. The default stays. | Restarts now |
| `kernel bench <suite> --all [--seconds <s>] [--dry-run]`, `kernel bench <suite> --cancel` | A series (a job): each kernel starts once, the suite runs after login, then the default kernel starts again. It stops at a kernel that does not start. Without `--all`: the newest run on each kernel, side by side (a read). | Restarts now, once per kernel |
| `kernel build <base> [--config-fragment <file>] [--name <suffix>] [--dry-run]` | Builds `linux-cachyos-<suffix>` from the installed base's PKGBUILD in a container (a job, about an hour), then `pacman -U` through the daemon. | Next start (`kernel try`) |
| `boot install\|confirm\|rebuild\|remove [--dry-run]` | legionctl's boot entry (systemd-boot and signed images). After `install`, the next start tries it once; `confirm` makes it first. | Next start |
| `greeter install\|remove [--dry-run]` | legionctl's greeter in place of GDM, or back. | Next start |
| `windows reboot [--dry-run]` | Restarts into Windows once. | Restarts now |
| `windows vm start\|stop`, `windows vm autostart off\|login\|boot` | The installed Windows in a VM (its own disk by VFIO). | Now |
| `cleanup [--unload-modules] [--dry-run]` | Turns off conflicting units of earlier Legion tooling. | Now |
| `tune <attribute> <scenario> <values>... [--seconds <s>] [--cooldown-s <s>] [--per-kernel]` | Steps a firmware power limit with a bench at each step; the original value comes back. Needs AC and an idle machine. The result is a layer (`tuned`, or `tuned-<package>` with `--per-kernel`), off until `rules enable`. | During the sweep |
| `undo <event>` | Turns back the writes of a machine change (each file is then held: `knobs release` gives it back), or a `display.change`. | Now |

Workspace-class tools (no polkit, but an agent's call asks the user):

- `bench run cpu|gpu|mixed [--seconds <s>] [--label <l>]`: loads the machine fully for the duration (1-600 s).
- `record <dir> [--minutes <m>]`: writes the files legionctl reads, the profile, the layers and recent events
  into a new directory. Use it for a bug report, a test fixture, or a record before and after a BIOS or kernel
  update.
- `virt act start|stop|pause|unpause <name>`, `virt tune <name> [--cores p|e|all] [--memory <size>] [--dry-run]`,
  `virt create <name> <image> [--cores ...] [--memory ...] [--gpu] [--dry-run]`.
- `windows vm view`: a SPICE window of the running Windows VM.

## How to track a change

1. `events`: each change is an event with the actor, the input and the result. `trace` follows the log live.
2. `knobs show <knob>`: the layer chain and the history of one knob.
3. `plan`: the writes the daemon would make now. An empty write list means the machine is as the layers want.
4. `doctor`: the findings after the change.
5. `bench list`, `bench compare`: scores before and after.
6. In the dossier: write the event, the before and after values, and the output into EVIDENCE.

## Scenario: the user wants a lower CPU power limit on AC

1. Read: `legionctl knobs show firmware.ppt_pl1_spl` and `legionctl plan`.
2. Propose the layer: `legionctl rules edit 50-quiet firmware.ppt_pl1_spl=90 --when power=ac`. A `power` layer
   must also set the knob for the other source (or a layer without `power`), else the check refuses it.
3. Give the user the command. The user runs it.
4. Check: `legionctl plan`, `legionctl knobs show firmware.ppt_pl1_spl`, `legionctl doctor`.
5. To turn it back: `legionctl rules edit 50-quiet firmware.ppt_pl1_spl=`, or `legionctl rules reset`.

## Scenario: a display finding

1. `legionctl display status` shows `[refresh-DP-3]`.
2. `legionctl display fix refresh-DP-3 --dry-run` shows the new and the old value.
3. With the user's agreement: `legionctl display fix refresh-DP-3`.
4. To turn it back: find its `display.change` event with `legionctl events`, then `legionctl undo <event>`.
