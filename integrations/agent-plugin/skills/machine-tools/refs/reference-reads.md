# Read-only machine commands

| Command | What it shows |
| --- | --- |
| `status [--fast]` | Power source, thermal mode (and the Fn+Q choice), firmware limits, CPU policy, temperature, GPU wiring, load. `--fast` reads sysfs and procfs only. |
| `load` | Load level (idle, normal, busy, overloaded) and why. |
| `doctor` | Health findings: policy drift, conflicting units, packages, modules, GPU wiring, display, layers that fail their checks. Each graphics finding has an id in brackets. |
| `monitor [--count <n>] [--interval-ms <ms>]` | Telemetry, one line per sample (count 1-3600, interval 100-60000 ms). |
| `sensors [--watch]` | Every sensor in one table with its unit and source (hwmon, firmware fan RPM, NVIDIA only while awake, RAPL, PSI, eBPF, MSR throttle reasons, fan controls, power source), with history. |
| `journalctl -o json -t legionctl-system LEGIONCTL_EVENT=<mode\|power-source\|workload\|throttle\|fan-mode>`, `busctl monitor org.legionctl.System1` | For other tools: the daemon's changes as journal entries and D-Bus signals. |
| `capabilities`, `validate` | Capability coverage from the profile; a check of the profile and the shipped layers (no hardware access). |
| `knobs list`, `knobs show <knob>` | Each knob: value now, the value the layers want, the firmware range, the policy; one knob's layer chain and history. |
| `plan`, `rules list`, `rules show <layer>` | The desired state now: conditions, the layers that apply, the writes the daemon would make. |
| `fan status`, `battery status` | Fan table, curve, fan max mode; battery mode, health, cycles, health trend. |
| `display status` | Monitors (mode, highest rate, scale, VRR, HDR), font antialiasing, findings with ids. |
| `gpu status`, `vfio status` | Which GPU drives each display and draws the desktop, the NVIDIA GPU's sleep state and the programs that keep it awake; which devices can go to a VM. |
| `firmware status` | The devices fwupd knows, their firmware version and the upgrade fwupd can apply. |
| `rgb status` | Whether OpenRGB answers and drives the keyboard, and its saved profiles (for knob `rgb.profile`). |
| `resources list` | Each resource (VMs, GPU, mounts with the processes that use them, layers, held knobs, saved sets, sessions, workspaces), its state and its actions with the operation each runs. |
| `kernel list`, `kernel bench <scenario>`, `boot status` | Installed kernels (scheduler, LTO, image, command line, DKMS state, running, default); the newest bench run on each kernel; firmware boot entries. |
| `greeter status`, `windows status`, `windows vm status`, `windows vm overlay status`, `windows restore list` | Login screen; dual boot and dirty NTFS volumes; the Windows VM; its repair overlay; Windows' restore points (guest agent). |
| `virt status` | Containers and VMs (WinBoat recognized) and what is wrong. |
| `bench list`, `bench compare <earlier> <later>`, `replay <dir>` | Bench runs; two runs side by side; what the daemon would write on a recorded machine. |
