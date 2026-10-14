---
name: tool-lifecycle
description: Index of every top-level legionctl command group - what it does, whether it only reads or changes something, how it runs (operator, root daemon with polkit, docker, QEMU, a job) and how to track it - with pointers to the skills workspace-practice, machine-tools, verify-and-ship and sessions-and-agents for depth. Use when you do not know which legionctl command fits a task, or before you use a command group for the first time.
user-invocable: true
---

# legionctl tool lifecycle

## How every command runs

1. The `legionctl` client sends the command to the operator (`legionctl-operator`, a user service on
   `$XDG_RUNTIME_DIR/legionctl/api.sock`). systemd starts it at the first call.
2. The operator runs reads as you. A machine change goes over D-Bus to the root daemon (`legionctl-system`), and
   polkit asks the human on this machine.
3. Each operation has a permission class (`docs/api/schema.json`):
   - `read`: runs at once, changes nothing.
   - `session`: changes only the caller's own session (its dossier, its workspace except merge).
   - `workspace`: changes your files, settings, containers or other sessions. An agent's call asks the user (a
     desktop notification, Allow or Deny, 120 s).
   - `machine`: the root daemon with a polkit dialog.
4. Each call that is not a read is an event (kind = the operation name) with the actor, the input and the result.
5. Each operation is also an MCP tool: the command path with `_` (`kernel cmdline set` is `kernel_cmdline_set`).
6. `--json` prints the operation's JSON output. `--dry-run`, where a command has it, shows the plan only.
7. Each command has `--help`. Read it before you use a flag; do not guess.

## Where to track

| Tool | What it shows |
| --- | --- |
| `events` | The event log, oldest first, in pages (`--cursor`, `--limit`). |
| `trace` | The event log live. |
| `jobs list`, `jobs show <id>` | Long operations and their results. |
| `tui` | A live dashboard. |
| The dossier | Your plan, decisions and EVIDENCE (skill `legionctl:dossier-practice`). |

## Command groups

Class: R read, S session, W workspace, M machine (polkit). A group with more than one class shows each.

| Group | What it does | Class | How it runs | Track with | Skill |
| --- | --- | --- | --- | --- | --- |
| `status` | Power source, thermal mode, limits, CPU policy, temperature, GPU wiring, load. | R | operator | `monitor`, `tui` | machine-tools |
| `load` | Load level and why. Check it before builds and tests. | R | operator | `load` | machine-tools |
| `doctor` | Health findings with ids. | R | operator | `doctor` again | machine-tools |
| `capabilities` | Capability coverage of the profile. | R | operator | - | machine-tools |
| `validate` | Checks the profile and the shipped layers (no hardware). | R | operator | - | verify-and-ship |
| `gpu` | `status`; `give` a GPU to Linux, Windows or no one; `primary` desktop GPU (next login). | R, M | daemon | `events`, `gpu status` | machine-tools |
| `display` | `status` of the monitors and fonts; `fix` your own display settings. | R, W | operator, as you | `events` (`display.change`), `undo` | machine-tools |
| `vfio` | `status`, `bind`, `unbind` of devices for a VM. | R, M | daemon | `vfio status` | machine-tools |
| `power` | `apply` the power policy (clears Fn+Q). | M | daemon | `events`, `plan` | machine-tools |
| `cleanup` | Turns off units of earlier Legion tooling. | M | daemon | `events`, `doctor` | machine-tools |
| `knobs` | `list`, `show`, `hold`, `release` the knobs. | R, M | daemon | `knobs show` (history) | machine-tools |
| `plan` | The desired state and the writes the daemon would make. | R | operator | - | machine-tools |
| `rules` | `list`, `show`, `enable`, `disable`, `edit`, `reset` the configset layers. | R, M | daemon (yours: as you, then a reload) | `rules list`, `plan` | machine-tools |
| `fan` | `status`, `curve set\|reset`, `max`, `clean`, `lock`. | R, M | daemon | `fan status`, `monitor` | machine-tools |
| `battery` | `status`, `mode`, `usb-charging`. | R, M | daemon | `battery status` | machine-tools |
| `boot` | `status`, `install`, `confirm`, `rebuild`, `remove` legionctl's boot entry. | R, M | daemon; next start | `boot status` | machine-tools |
| `kernel` | `list`, `default`, `try` (restarts), `cmdline set\|undo`. | R, M | daemon; next start | `kernel list` | machine-tools |
| `greeter` | `status`, `install`, `remove` legionctl's login screen. | R, M | daemon; next start | `greeter status` | machine-tools |
| `windows` | `status`, `reboot` (restarts), `vm start\|stop\|autostart\|view\|status`. | R, W, M | daemon, QEMU | `windows vm status` | machine-tools |
| `virt` | `status`, `act`, `tune`, `create` containers and VMs. | R, W | docker, virsh, as you | `virt status` | machine-tools |
| `undo` | Turns back a change from the event log. | M | daemon (`display.change`: as you) | `events` | machine-tools |
| `record` | Records this machine into a directory. | W | operator | the directory | machine-tools |
| `replay` | What the daemon and doctor do on a recorded machine. | R | operator | - | machine-tools |
| `bench` | `run` a workload with telemetry; `list`; `compare`. | W, R | operator, stress-ng, gpu-burn | `bench list`, `bench compare` | machine-tools |
| `tune` | Steps a firmware limit with a bench at each step. | M | daemon, minutes | `events`, `rules enable tuned` | machine-tools |
| `monitor` | Telemetry, one line per sample. | R | operator | - | machine-tools |
| `dossier` | `use`, `active`, `release`, `locks`, `list`, `read`, `search`, `write`, `append`, `edit`, `add`, `check`, `graph`, `nudges`. | R, S | operator | `dossier check`, `dossier graph`, `dossier nudges` | dossier-practice |
| `workspace` | `open`, `status`, `list`, `checkpoint`, `merge`, `close`; `pack` an agent image. | R, S, W | operator, git; `pack`: docker or QEMU, a job | `workspace status`, `workspace list` | workspace-practice |
| `session` | `start`, `run`, `list`, `stop`, `activity`, `pause`, `unpause`, `resume` agent sessions. | R, W | systemd scope, bwrap, docker or QEMU; `run` is a job | `session list`, `session activity`, `jobs`, `trace` | sessions-and-agents |
| `jobs` | `list`, `show` long operations. | R | operator | - | sessions-and-agents |
| `tools` | `calls`: MCP calls of session agents. | R | operator | - | sessions-and-agents |
| `agents` | `list`, `set` agent settings; `accounts list\|add`; `plugins list\|install`; `usage`. | R, W | operator, as you | `agents usage` | sessions-and-agents |
| `gateway` | `usage` of the model gateway. | R | operator | - | sessions-and-agents |
| `mcp-proxy` | A session agent's way to an MCP server: allowed tools only, each call logged. | - | client only | `tools calls` | sessions-and-agents |
| `packages` | `list`, `fetch` packages into the store (pinned hash). | R, W | operator, network | `packages list` | sessions-and-agents |
| `cache` | `stats`, `purge` the disposable cache. | R, W | operator | `cache stats` | sessions-and-agents |
| `images` | `scan`, `build`, `run`, `status` a repository's Docker services. | W, R | docker buildx, docker compose; `build` is a job | `images status`, `jobs` | sessions-and-agents |
| `setup` | `check`, `apply` ctx and legionctl for Claude Code and Codex. | R, W | operator, user config files with backups | `setup check` | - |
| `reminders` | `list`, `on`, `off`, `add`, `rm` the reminders the hooks inject. | R, W | operator | `reminders list` | - |
| `hooks` | `list`, `bind`, `unbind`, `add`, `rm` hook actions. | R, W | operator | `hooks list` | workspace-practice |
| `skills` | The legionctl skills, where each agent has them, and their reminders. | R | operator | - | - |
| `config` | `show`, `explain`, `layers` of the merged config. | R | operator | `config explain <key>` | - |
| `events` | The event log. | R | operator | - | - |
| `trace` | The event log live. | - | client only | - | - |
| `tui` | A live dashboard. | - | client only | - | - |
| `mcp` | The MCP server over stdio for agents. | - | client only | `events` | - |
| `hook` | The Claude Code hook adapter (`hooks/hooks.json` calls it). | - | client only | `dossier nudges` | workspace-practice |
| `run` | Runs a program on a chosen GPU (`--gpu`), core class (`--cores`) and `--nice`. | - | client only, as you | - | machine-tools |

## Lifecycle of a task

1. Bind: `dossier use <name>` (skill `legionctl:dossier-practice`).
2. Open: `workspace open [--topic <t>]` (skill `legionctl:workspace-practice`).
3. Read the machine when the task needs it: `status`, `doctor`, `plan` (skill `legionctl:machine-tools`).
4. Give work away when it must be isolated or headless: `session run` (skill `legionctl:sessions-and-agents`).
5. Verify: build, lint, tests, mutation checks, EVIDENCE (skill `legionctl:verify-and-ship`).
6. Commit: `workspace checkpoint "<message>"`, and record it in the dossier.
7. Merge with the user's approval: `workspace merge`. Close: `workspace close`.
