# Machine changes

Class M unless the row says W (workspace: no polkit, an agent's call asks the user). `--help` shows which
commands take `--dry-run`.

| Command | What it changes | When it applies |
| --- | --- | --- |
| `power apply [--source auto\|ac\|battery]` | The power policy. It clears an Fn+Q choice. | Now |
| `knobs hold <knob> <value>`, `knobs release <knob>` | Holds a knob against the policy; gives it back to the policy. | Now, until release |
| `rules edit <layer> [knob=value]... [--when <conditions>] [--system]` | Your configset layer (or a system layer). `knob=` removes a knob. Conditions: `power`, `fnq`, `workload`, `kernel`, or `always`. | Now |
| `rules enable\|disable <layer>`, `rules reset [--knob <knob>] [--system]` | A layer on or off (`rules enable tuned` uses the `tune` result); back to the shipped state. | Now |
| `rules sets save <name> [--boot]`, `rules sets switch <name>`, `rules sets remove <name>` | A saved set of layer states and holds. A switch is one change; `switch previous` is its undo. `--boot` adds a hidden boot entry for each kernel (needs `boot install`). | Now; a boot entry at the next start |
| `fan curve set <10 levels>`, `fan curve reset`, `fan max on\|off`, `fan clean on\|off` | Fan curve (custom mode only), fan max mode, dust-clean speed. The fan table and full speed are not available on this machine (`--dry-run` shows the writes). | Now; set again after resume |
| `battery mode standard\|conservation\|rapid`, `battery usb-charging on\|off` | Charge mode; USB power while off. | Now |
| `gpu primary nvidia\|intel\|auto [--release-nvidia]` | The GPU that draws the desktop. | Next login |
| `gpu power off\|on` | off: the NVIDIA GPU sleeps (D3cold) when no program uses it; refused while one does. on: it stays powered. | Now |
| `firmware update <id\|name>` | One device's firmware through fwupd (fwupd's own polkit dialog). No restart; some updates finish at the next start. | Now or next start |
| `overlay on\|off` (W) | The vitals overlay on the desktop. | Now |
| knob `rgb.profile` (in a layer: `rules edit`) | The keyboard light: a saved OpenRGB profile, loaded when its conditions change. | At the next policy event |
| `gpu give igpu\|nvidia linux\|windows\|none`, `vfio bind\|unbind <devices>` | A GPU or an IOMMU group to Linux, to the Windows VM, or to no one. Apps on that GPU lose it. | Now |
| `display fix <id>\|all [--persistent]` (W) | Your own settings only: antialiasing, an app's flags file, a monitor's rate, the session environment. | Now; a mode until next login without `--persistent` |
| `kernel default <name>` | The default kernel. Needs `boot install` first. | Next start |
| `kernel cmdline set <name> "<line>"`, `kernel cmdline undo <name>` | One kernel's command line. The first call makes a signed trial image and sets the next start only to it; the same call after a good start through it makes it the kernel's image. Needs `boot install` first. | Next start (trial), then the second call |
| `kernel try <name>` | Restarts into the kernel once. The default stays. | Restarts now |
| `kernel bench <scenario> --all [--seconds <s>]`, `--cancel` | A job: each kernel starts once, the suite runs after login, then the default kernel starts again. It stops at a kernel that does not start. | Restarts now, once per kernel |
| `kernel build <base> [--config-fragment <file>] [--name <suffix>]` | Builds `linux-cachyos-<suffix>` from the installed base's PKGBUILD in a container (a job, about an hour), then `pacman -U` through the daemon. | Next start (`kernel try`) |
| `boot install\|rebuild\|remove` | legionctl's boot entry (systemd-boot and signed images). After `install`, the next start tries it once; when that start reaches the login screen, the entry becomes the default by itself. | Next start |
| `greeter install\|remove` | legionctl's greeter in place of GDM, or back. | Next start |
| `windows reboot` | Restarts into Windows once. | Restarts now |
| `windows vm start\|stop\|pause\|resume`, `windows vm autostart off\|login\|boot`, `windows vm view` (W) | The installed Windows in a VM (its own disk by VFIO); pause and resume stop and continue its CPUs (QMP); view opens a SPICE window. | Now |
| `windows disk attach\|detach` | The Windows disk's partitions to Linux (automounts start) or away (automounts and mounts stop). Refused while the VM runs. | Now |
| `windows vm overlay create\|drop\|commit`, `windows vm image <file>` | Repairs run on a copy-on-write overlay: while it exists each VM start writes to it and the disk stays as it is; drop leaves the disk as it was; commit writes it into the disk (polkit). The image is a full copy (only read). Refused while the VM runs. Never commit without the user. | Now |
| `windows vm recovery`, `windows repair sfc\|dism\|chkdsk\|boot`, `windows restore apply <n>` | In the Windows VM on an overlay only: WinRE at the next start, repairs through the guest agent (output as events), bcdboot typed into WinRE, a System Restore point. | Now (in the VM) |
| `resources act <id> <action>` | Runs the action's own operation (see `resources list`), with its class, its dialog and its event. | As that operation |
| `cleanup [--unload-modules]` | Turns off conflicting units of earlier Legion tooling. | Now |
| `tune <attribute> <scenario> <values>... [--seconds <s>] [--cooldown-s <s>] [--per-kernel]` | Steps a firmware power limit with a bench at each step; the original value comes back. Needs AC and an idle machine. The result is a layer (`tuned`, or `tuned-<package>`), off until `rules enable`. | During the sweep |
| `undo <event>` | Turns back the writes of a machine change (each file is then held: `knobs release` gives it back), or a `display.change`. | Now |
| `bench run cpu\|gpu\|mixed [--seconds <s>] [--label <l>]` (W) | Loads the machine fully for 1-600 s. | During the run |
| `record <dir> [--minutes <m>]` (W) | Writes the files legionctl reads, the profile, the layers and recent events into a new directory: for a bug report, a test fixture, or before and after a BIOS or kernel update. | Now |
| `virt act start\|stop\|pause\|unpause <name>`, `virt tune <name> [--cores p\|e\|all] [--memory <size>]`, `virt create <name> <image> [--cores] [--memory] [--gpu]` (W) | A container or VM. | Now |
| `fan lock on\|off` | A raw EC write. Do not use. | - |
