# Lenovo Legion Control

Local control package for this machine:

- `LENOVO 82WQ`
- `Legion Pro 7 16IRX8H`
- observed BIOS: `KWCN54WW`
- observed BIOS release: `1.54`
- observed EC firmware release: `1.51`
- observed board name: `INVALID`

This repository is intentionally machine-specific. It is not a generic Legion
support package and it does not try to guess safe behavior for other models.

## Scope

Owned here:

- AC and battery power policy
- Intel RAPL package limits through standard `powercap` sysfs
- CPU EPP/governor/turbo policy
- Lenovo firmware thermal profile through `platform_profile`
- VPC2004 fan mode status
- NVIDIA persistence and `nvidia-powerd` integration
- power-profiles-daemon profile sync (PPD keeps EPP and the GNOME Power Mode UI)
- system sysctl set (profile `sysctl`, per-mode overrides in `ac.sysctl`/`battery.sysctl`)
- sched_ext scheduler config (`tuning/scx_loader.toml`: `scx_lavd --autopower`)
- conflict/status checks and profile validation

Not owned here:

- NTFS, fstab, or mount management
- GUI
- RGB
- custom fan curves
- direct `thinkpad_acpi` fan control
- blanket blacklisting of in-tree Lenovo kernel drivers
- replacing `nvidia-powerd`

## Commands

Run read-only status checks:

```bash
./src/legionctl/legionctl status
./src/legionctl/legionctl doctor
./src/legionctl/legionctl capabilities
./src/legionctl/legionctl cleanup --dry-run
./src/legionctl/legionctl validate          # profile structure, no hardware access
./src/legionctl/legionctl gpu               # which GPU drives each display; hybrid vs discrete
```

`status` also shows firmware power limits, fan RPM (root, via `acpi_call`), logged throttle reasons (root) and the
Fn+Q override. `doctor` adds kernel module/DKMS inventory, dirty NTFS volumes and desktop-GPU placement.

Choose the GPU that draws the desktop (applies at next login), and launch programs on a GPU, core class and priority:

```bash
sudo legionctl gpu primary nvidia            # or intel, or auto
legionctl run --gpu nvidia -- blender
legionctl run --cores e --nice 10 -- make -j16
```

Fn+Q is followed automatically (`legionctl follow`, run by udev); the choice holds until the power source changes,
reboot, or `apply`. After resume, `/usr/lib/systemd/system-sleep/legion` re-applies policy and keeps that choice.

Capture hotkey/WMI/display/fan evidence:

```bash
sudo -E ./scripts/capture-capabilities.sh
```

Apply the AC policy:

```bash
sudo ./src/legionctl/legionctl apply ac
```

Apply the current AC/battery mode automatically:

```bash
sudo ./src/legionctl/legionctl apply auto
```

Dry-run an apply:

```bash
./src/legionctl/legionctl apply ac --dry-run
```

Disable old local power-management units:

```bash
sudo legionctl cleanup
```

Package removal and obsolete module unloading are intentionally explicit:

```bash
sudo legionctl cleanup --unload-modules
sudo legionctl cleanup --remove-packages
```

## Install

For a direct local install:

```bash
sudo make install
sudo udevadm control --reload
sudo systemctl daemon-reload
sudo systemctl enable --now legion-apply.service legion-reassert-ac.timer
sudo systemctl start legion-apply.service
```

Or run the combined install/apply/check helper:

```bash
sudo ./scripts/install-apply-check.sh
```

For Arch/CachyOS packaging:

```bash
cd packaging/arch
makepkg -si
```

Then enable the same services:

```bash
sudo systemctl enable --now legion-apply.service legion-reassert-ac.timer
sudo systemctl start legion-apply.service
```

## Current AC Targets

- CPU nominal metadata: 55 W
- CPU sustained/loaded long: 70 W
- CPU short: 125 W
- CPU peak: 125 W
- GPU: firmware-managed Dynamic Boost through `nvidia-powerd`, expected 150 W
- Firmware thermal profile: `max-power` when available
- VPC2004 fan mode: status-only; writing `4` immediately reads back as `0`

## Capability Coverage

See [docs/CAPABILITIES.md](docs/CAPABILITIES.md) for the machine capability
inventory. The current installed scope is CPU/GPU/firmware performance policy.
Hotkeys such as `Fn+Q`, `Fn+R`, `Fn+Space`, real fan telemetry/control, RGB,
hybrid graphics, G-Sync, and panel overdrive are tracked there as separate
capabilities until they are verified on this 82WQ machine.

See [docs/REFERENCE_COMPARISON.md](docs/REFERENCE_COMPARISON.md) for the
baseline-vs-reference comparison against the three upstream repositories.

See [docs/MEASUREMENT.md](docs/MEASUREMENT.md) for the capture workflow that
feeds the next DKMS and hotkey work.

## Development

`make` is the entry point; run `make help` for every target.

```bash
make tools      # uv-installed pre-commit plus hook environments (ruff, mypy, shellcheck, shfmt, actionlint)
make hooks      # also install the git pre-commit hook
make fmt        # format Python and shell in place
make lint       # every hook on all tracked files (what CI runs, plus `make validate`)
make validate   # profile structure and ac/battery consistency, no hardware access
make check      # validate + live status on this machine
make lint-pkg   # namcap the PKGBUILD (Arch only)
```

Measure before changing sustained CPU power: `sudo ./scripts/rapl-sweep.sh` sweeps RAPL PL1 and
writes a CSV of throughput, package power, temperature, clocks and fan speed per step.

## Operator CLI, TUI and agent plugin

`legion` (Rust) shows this machine's state and load, manages isolated git worktrees, checks Claude Code and Codex
settings, and requests machine changes that a human approves in a polkit dialog. The same functions are exposed to
agents as an MCP server.

```bash
make build && sudo make install      # builds and installs /usr/bin/legion with the rest
legion status                        # power, thermal mode, limits, GPU wiring, load
legion load --brief                  # one-line load level for scripts and agent hooks
legion worktree create my-task --repo ~/code/project
legion agents                        # Claude Code / Codex settings: present, valid, sections
legion act set-desktop-gpu nvidia    # asks for approval first
legion tui                           # live dashboard
```

Claude Code plugin: MCP tools; skills `legion:ste100-style` (ASD-STE100 writing) and `legion:glanceable-style`
(glanceable, agent-readable output); hooks that inject reminders, load and setup notices; commands `/legion:status`,
`/legion:worktree`, `/legion:act`, `/legion:setup`, `/legion:reminders` (guided), and `/legion:skills`
(list skills and where they are installed, load one, turn its reminder on or off; also `legion skills`). Reminders also work from the shell:
`legion reminders`, `legion reminders on|off <id>`, `legion reminders add <id> "<text>" [--event prompt]`.
What the hooks run is configurable: `/legion:hooks` (guided) or `legion hooks`, `legion hooks bind|unbind <event> <action>`,
`legion hooks add <id> "<command>" [--timeout N]`. Bindings and an event log of every change live in
`~/.local/state/legion/legion.db` (SQLite, WAL).

```
/plugin marketplace add /data/git/lenovolegionlinux
/plugin install legion@lenovolegionlinux
```

Codex: append `integrations/codex/config.toml` to `~/.codex/config.toml`.
