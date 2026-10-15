# legionctl

One operator for this machine and the coding agents that run on it. It controls power, firmware, GPU and display
policy with the in-tree Lenovo drivers (no out-of-tree module), gives developers tools to see and check the system,
and gives Claude Code and Codex hooks, skills, reminders, dossiers, workspaces and an MCP server. Design notes:
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

State: `/var/lib/legionctl/system.db` (machine: the Fn+Q ledger), `~/.local/state/legionctl/operator.db` (event log),
`~/agentWorkspaces/<dossier>/` (agent dossiers, each with its own `.state/dossier.db`). Configuration is files only
(see Agents).
Machine policy: `profiles/lenovo-82wq-legion-pro-7-16irx8h.json` (JSONC).

## Install

Install with the Arch package, so pacman tracks every file:

```bash
make install-package    # the first time: build this checkout's package, install it with sudo (password)
sudo systemctl enable --now legionctl-system.service scx_loader.service
systemctl --user enable --now legionctl-operator.socket legionctl-operator.service
gnome-extensions enable legionctl-osd@legionctl    # takes effect at the next login
```

After the first install, `make install-package` runs `sudo -n /usr/lib/legionctl/legionctl-install` with no password
(`/etc/sudoers.d/legionctl-install`, group `wheel`, no arguments). The wrapper installs only the newest commit of
`origin/main` on GitHub when its CI run passed: the user `legionctl-build` clones and builds it in
`/var/lib/legionctl-build`, root installs that file and restarts your operator. Journal: `journalctl -t legionctl-install`.
Limits: code on `main` is written by agents; the wrapper refuses only a commit that is not on `main` or that has no
passed CI run. `legionctl-build` is in the group `docker` (the static client builds in a container), which is equal to
root for that user. The sudo timestamp of this machine (`timestamp_type=global`, 20 minutes) does not change. Details:
the skill verify-and-ship, `refs/reference-install.md`.

## Commands

```bash
legionctl-app                     # the desktop app: glance, policy preview, GPU wiring, health, bench
legionctl status                  # power, mode, CPU, limits, GPU wiring, units, modules, load (fans and throttle: root)
legionctl doctor                  # drift, conflicts, kernel modules and DKMS, NTFS, GPU placement, failed units, OSD, agent setup
legionctl knobs list              # every knob: value, what the profile wants now, firmware range, policy
legionctl knobs show cpu.epp      # one knob and the events that changed it
legionctl record ~/rec-bios       # the files legionctl reads, the profile and machine events: a bug report or fixture
legionctl replay ~/rec-bios       # what the daemon and doctor decide on that recording
legionctl bench run cpu --seconds 30 --label pl1-90   # stress-ng/gpu-burn with telemetry; bench compare <earlier> <later>
legionctl tune ppt_pl1_spl cpu 60 75 90                # sweep a firmware limit (polkit); recommends the knee
legionctl load                    # one-line load level, and which cgroup waits on I/O
legionctl monitor --count 10      # power, temperatures, clocks, GPU, PSI, fans, throttle reasons (GET /v1/metrics too)
legionctl trace                   # follow the event log: agents, people and the machine daemon
legionctl session run claude "fix the failing test"   # headless: the agent over ACP in the session sandbox
legionctl windows status          # the Windows boot entry and what Windows left dirty; windows reboot restarts into it once
legionctl gpu status              # which GPU drives each display; hybrid or discrete
legionctl run --gpu nvidia -- blender
legionctl run --cores e --nice 10 -- make -j16
legionctl power apply --dry-run   # what the policy would change now
legionctl power apply             # through the daemon; polkit asks you
legionctl gpu primary nvidia             # applies at the next login
legionctl validate                # profile structure, no hardware access
legionctl config layers           # config files in merge order; `config explain <key>` says which file set a value
legionctl dossier list            # agent dossiers in ~/agentWorkspaces (agents bind them; see Agents)
legionctl dossier migrate --dry-run  # once, from ~/.agentworkspace: what moves; then without --dry-run to move
legionctl workspace list          # each dossier's git worktree: path, branch, base
legionctl session start claude    # Claude Code in a sandbox on the dossier's workspace: home read-only, git gated
legionctl tui                     # live dashboard
legionctl --help                  # every operation; add --json to any of them
```

The policy: AC uses the firmware's `max-power` mode, battery uses `balanced`; power limits are the firmware's own
(note 02). Fn+Q changes the mode until the charger is plugged in or out, a reboot, or `legionctl power apply`.

## Agents

Claude Code plugin (`/plugin marketplace add /data/git/lenovolegionlinux`, then
`/plugin install legionctl@lenovolegionlinux`):

- one MCP tool per operation (status, load, doctor, power, GPU, cleanup, workspaces, setup, reminders, hooks, skills,
  config, dossiers);
- skills `legionctl:ste100-style`, `legionctl:glanceable-style` and `legionctl:dossier-practice`, kept in force by
  reminders, and the tool skills `legionctl:tool-lifecycle` (an index of every command group),
  `legionctl:workspace-practice`, `legionctl:machine-tools`, `legionctl:verify-and-ship` and
  `legionctl:sessions-and-agents`, loaded when the task matches;
- hooks (`legionctl hook <event>`): reminders, load and setup notices; one dossier per session, bound again at
  session start where the last session worked; the agent is told first when a session has none (`dossier.nudge`),
  prompts are blocked until it has one (`dossier.gate`), and writes into another session's dossier are denied;
- workspaces: a git worktree per dossier and repository, and one per topic for parallel agents
  (`/legionctl:workspace`: open, status, checkpoint, merge, close); the agent is told to open one at session start and when it changed the main checkout instead;
- commands `/legionctl:dossier`, `/legionctl:workspace`, `/legionctl:status`, `/legionctl:act`, `/legionctl:setup`,
  `/legionctl:reminders`, `/legionctl:hooks`, `/legionctl:skills`.

Settings (workspace root, reminders, hook bindings, load thresholds, dossier pattern and gate) are JSONC config:
built-in defaults (`config/defaults.jsonc`), then `/etc/legionctl`, `~/.config/legionctl` and `<repo>/.legionctl`.

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
make package    # build the Arch package of this checkout (`make install-package` installs origin/main, see Install)
```

`scripts/capture-capabilities.sh` records hotkey, WMI, display, fan and power evidence (until `legionctl record`
exists, note 16).
