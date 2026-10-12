# 15 — Layout: one name, one API, one operator

Decisions (2026-10-12):

- `legionctl` is the only name. Components use `legionctl-*`.
- The operator is the only front door. Every capability is one operation, and all surfaces are adapters to it.
- One path for each capability: no fallbacks, no parallel implementations, no compatibility shims. A failed path
  puts the engine in safe mode and reports why.
- GPUI is the only graphical UI. There is no web UI.
- Tests are end-to-end at a real boundary, or they catch a failure that is not obvious. Other tests are deleted.

This note is the authority for names, processes, crates and paths. Notes 00–14 use these names.

## Processes

```
 clients (no state)                                engines (own state, restartable)
 legionctl       CLI · TUI · MCP · hook dispatch ─┐
 legionctl-app   GPUI desktop                     ├─ api.sock ─▶ legionctl-operator (user) ─ D-Bus + polkit ─▶ legionctl-system (root)
 agent plugin    /legionctl:*                     │              operator.db, approvals,       ◀── events ──────  system.db, firmware, power,
 GNOME extension OSD ◀────────────────────────────┘              agents, sessions, tools,                         GPU, kernel, eBPF, sysctl,
                                                                 workspaces, config, event log                    input, display
                                                     legionctl-sandbox: per session, bwrap + git shim, MCP proxy
                                                     legionctl-bootstrap (oneshot, system and user): install, upgrade, boot, migrations
```

| Process | Runs as | Owns | Interface |
| --- | --- | --- | --- |
| `legionctl-system` | root, system service | machine state, firmware, power, GPU, display policy, kernel, eBPF, Fn keys | D-Bus `org.legionctl.System1`, polkit per method |
| `legionctl-operator` | user service | operator database, operation registry, approvals, agents, sessions, tools, workspaces, config, OSD and Mutter calls when a graphical session exists | `$XDG_RUNTIME_DIR/legionctl/api.sock` (axum, HTTP + WebSocket) |
| `legionctl-sandbox` | user, one per session scope | bwrap sandbox, gated-command shim, MCP proxy | stdio to the agent |
| `legionctl-bootstrap` | oneshot, system and user | checks, migrations with backup, version marker, safe mode | none |
| `legionctl` | user | nothing | CLI, `legionctl tui`, `legionctl mcp`, `legionctl hooks run` |
| `legionctl-app` | user | nothing | GPUI |

Why these boundaries:

- Privilege: the root binary links only machine code. No HTTP server, model client or package fetcher runs as root.
- Failure isolation: Fn keys and power policy keep working when the operator restarts. Agents keep their model
  calls when the operator restarts.
- Clients have one path: the operator API. When the operator is not running, a client says so and offers to start
  it. Clients never read the databases or sysfs directly.

## One operation registry

Each operation is declared once in `legionctl-api`: name, input and output schemas, permission class (read,
workspace, machine, account) and engine. The registry generates every surface:

| From `power.policy.set` | Surface |
| --- | --- |
| HTTP | `POST /v1/ops/power.policy.set` |
| MCP tool | `power_policy_set` |
| CLI | `legionctl power policy set ac` |
| polkit action | `org.legionctl.power.policy.set` |
| event log kind | `power.policy.set` with actor, input and outcome |
| TUI and GPUI | view models read the same schema |

A surface cannot have an operation that the others do not have.

## Crates

```
crates/
  legionctl-core      ids, errors, config layers (precedence, locks, explain), event log, one-writer database, migrations
  legionctl-api       operation registry, schemas, API version, unix-socket client
  legionctl-machine   sysroot (paths over /sys, /proc, /dev, /run), sensors, actuators, firmware attributes, power,
                      cpufreq, PPD, sched_ext, GPU, display, fans, input, sysctl, kernel modules, DKMS, initramfs,
                      disks, boot, windows
  legionctl-bpf       aya programs and loader; observation only; pinned in bpffs
  legionctl-operator  gateway (19), agents (settings, skills, reminders, hooks, setup, accounts), sessions (ACP, workdirs, git state,
                      lifecycle), tools (package store, MCP proxy, call cache), workspaces (18), approvals, sagas, session OSD
  legionctl-ui        view models and disclosure levels for the TUI and GPUI
bins/
  legionctl  legionctl-system  legionctl-operator  legionctl-bootstrap  legionctl-sandbox  legionctl-app
```

A module becomes its own crate only when its size or its dependencies require it. Until then it stays a module.

## Names and paths

| Kind | Name |
| --- | --- |
| binaries | `/usr/bin/legionctl`, `/usr/bin/legionctl-app`; `/usr/lib/legionctl/legionctl-{system,operator,bootstrap,sandbox}` |
| system units | `legionctl-system.service`, `legionctl-bootstrap.service`, `legionctl-safe.service` |
| user units | `legionctl-operator.service`, `legionctl-session-<id>.scope`, `legionctl-session-<id>.timer` |
| D-Bus, polkit | `org.legionctl.System1`, actions `org.legionctl.<operation>` |
| config layers | built-in defaults (`config/defaults.jsonc`) → `/etc/legionctl` → `~/.config/legionctl` → `<repo>/.legionctl` → session; JSONC (14) |
| state | `/var/lib/legionctl/system.db`, `~/.local/state/legionctl/operator.db`, `~/.local/share/legionctl/{store,sessions}`, `~/.cache/legionctl/{gateway,tools}` |
| runtime | `/run/legionctl/`, `$XDG_RUNTIME_DIR/legionctl/`, `/sys/fs/bpf/legionctl/` |
| udev, hwdb | `99-legionctl.rules`, `90-legionctl-keys.hwdb` |
| GNOME extension | `legionctl-osd@legionctl` |
| agents | plugin `legionctl` (`/legionctl:status`, `/legionctl:skills`, ...), MCP server `legionctl`, env `LEGIONCTL_*` |
| worktrees | branch `legionctl/<name>` |

## Command tree

| Area | Commands | Engine |
| --- | --- | --- |
| overview | `status`, `doctor`, `load`, `events`, `approve` | operator |
| power and thermal | `power` (policy, profile, limits, tune), `thermal`, `fan` | system |
| graphics and display | `gpu` (mode, desktop GPU, pinning, run on), `display` (refresh, primary GPU) | system, operator for Mutter |
| kernel and OS | `kernel` (modules, DKMS, initramfs, sysctl, sched_ext), `disk`, `boot`, `windows` | system |
| input | `input` (Fn keys, Logitech devices) | system |
| virtualization | `virt` (status, list, create, start, stop, pause, tune), 17-virtualization | operator, system for kernel parts |
| config | `config` (layers, explain, edit, lock) | operator, system for `/etc` |
| agents | `agents`, `skills`, `reminders`, `hooks`, `tools` | operator |
| work | `dossier` (use, add, read, edit, search, check, graph), `workspace` (open, new branch, checkpoint, merge, close, pack, run; replaces `worktree`), `sessions`, `gateway` | operator, gateway |
| surfaces | `tui`, `mcp`, `app` | client |

## Switch from the Python tooling

Done 2026-10-13, in one change: the parity gate passed (`make parity`, 12 cases) and the change deleted
`src/legionctl`, `src/legion-osd`, `src/legion-refresh-toggle`, the reassert timer, the follow and apply services,
the AC/battery targets, the udev `RUN+=` rules, the sleep hook and the hwdb remap, renamed the client binary to
`legionctl`, moved the Fn+Q record to `system.db`, and made the Arch package the install path.

## Tests

- End-to-end at a boundary: the CLI against a temporary operator database, the daemon against a recorded sysroot
  of this machine (`tests/fixtures/sysroot-82wq-*`), the MCP server over stdio, the hooks through the plugin's real
  commands.
- Narrow tests only where they catch a failure that is not obvious, such as phantom I/O pressure (iowait
  with idle disks) or a migration that runs twice.
- Tests that only restate the code are deleted.
