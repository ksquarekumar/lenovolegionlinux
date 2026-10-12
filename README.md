# legionctl

One operator for this machine and the coding agents that run on it. It controls power, firmware, GPU and display
policy with the in-tree Lenovo drivers (no out-of-tree module), gives developers tools to see and check the system,
and gives Claude Code and Codex hooks, skills, reminders, worktrees and an MCP server. Design notes:
[docs/design](docs/design/README.md) (start with 15-layout).

This repository is for one machine only. It does not guess safe behavior for other models.

| | |
| --- | --- |
| Model | `LENOVO 82WQ`, Legion Pro 7 16IRX8H |
| BIOS / EC | `KWCN54WW` (1.54) / 1.51 |

## Parts

| Part | Runs as | Does |
| --- | --- | --- |
| `legionctl` | you | CLI, TUI (`legionctl tui`) and MCP server (`legionctl mcp`); each command is an operation of the operator |
| `legionctl-system` | root, `legionctl-system.service` | power policy on boot, charger plug/unplug, Fn+Q and resume; Fn+R key; D-Bus API `org.legionctl.System1` (polkit decides each change) |
| `legionctl-operator` | you, `legionctl-operator.socket` and `.service` (user) | the operation API on `$XDG_RUNTIME_DIR/legionctl/api.sock` (reads, plans, agent tooling, event log; machine changes go to `legionctl-system`); Fn+R through Mutter; OSD requests |
| `legionctl-osd@legionctl` | GNOME Shell extension | the animated OSD: thermal mode, panel refresh rate, Fn lock |

State: `/var/lib/legionctl/system.db` (machine), `~/.local/state/legionctl/operator.db` (agent hooks, event log).
Machine policy: `profiles/lenovo-82wq-legion-pro-7-16irx8h.json` (JSONC).

## Install

Install with the Arch package, so pacman tracks every file:

```bash
make package
sudo systemctl enable --now legionctl-system.service scx_loader.service
systemctl --user enable --now legionctl-operator.socket legionctl-operator.service
gnome-extensions enable legionctl-osd@legionctl    # takes effect at the next login
```

## Commands

```bash
legionctl status                  # power, mode, CPU, limits, GPU wiring, units, modules, load (fans and throttle: root)
legionctl doctor                  # drift, conflicts, kernel modules and DKMS, NTFS, GPU placement
legionctl load                    # one-line load level, and which cgroup waits on I/O
legionctl gpu status              # which GPU drives each display; hybrid or discrete
legionctl run --gpu nvidia -- blender
legionctl run --cores e --nice 10 -- make -j16
legionctl power apply --dry-run   # what the policy would change now
legionctl power apply             # through the daemon; polkit asks you
legionctl gpu primary --choice nvidia    # applies at the next login
legionctl validate                # profile structure, no hardware access
legionctl tui                     # live dashboard
legionctl --help                  # every operation; add --json to any of them
```

The policy: AC uses the firmware's `max-power` mode, battery uses `balanced`; power limits are the firmware's own
(note 02). Fn+Q changes the mode until the charger is plugged in or out, a reboot, or `legionctl power apply`.

## Agents

Claude Code plugin (`/plugin marketplace add /data/git/lenovolegionlinux`, then
`/plugin install legionctl@lenovolegionlinux`):

- one MCP tool per operation (status, load, doctor, power, GPU, cleanup, worktrees, setup, reminders, hooks, skills);
- skills `legionctl:ste100-style` and `legionctl:glanceable-style`, kept in force by reminders;
- hooks that inject reminders, load and setup notices; commands `/legionctl:status`, `/legionctl:worktree`,
  `/legionctl:act`, `/legionctl:setup`, `/legionctl:reminders`, `/legionctl:hooks`, `/legionctl:skills`.

Codex: `legionctl setup apply` adds the MCP server to `~/.codex/config.toml` and links the skills.

## Development

`make help` lists every target.

```bash
make tools      # pre-commit and its hooks, Biome and tsc (bun), cargo-machete, cargo-deny
make lint       # every pre-commit hook on all files
make rust-lint  # rustfmt check, clippy (pedantic) with warnings as errors, cargo-machete, cargo-deny
make js-lint    # Biome and tsc for the GNOME Shell extension
make test       # end-to-end tests: the CLI and MCP against the real operator, the daemon against a recorded copy of this machine
make ci         # what CI runs
make package    # build and install the Arch package
```

`scripts/capture-capabilities.sh` records hotkey, WMI, display, fan and power evidence (until `legionctl record`
exists, note 16).
