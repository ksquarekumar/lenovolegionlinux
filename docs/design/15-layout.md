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
 legionctl       CLI · MCP · hook adapter ─┐
 legionctl-app   GPUI desktop                     ├─ api.sock ─▶ legionctl-operator (user) ─ D-Bus + polkit ─▶ legionctl-system (root)
 agent plugin    /legionctl:*                     │              operator.db, approvals,       ◀── events ──────  system.db, firmware, power,
 GNOME extension OSD ◀────────────────────────────┘              agents, sessions, tools,                         GPU, kernel, eBPF, sysctl,
                                                                 workspaces, config, event log                    input, display
 legionctl-greeter  greetd greeter (cage) ──────── D-Bus + polkit (user greeter) ──────────────────────────────▶ legionctl-system
                                                     sessions: bwrap sandbox, git gate and `legionctl mcp-proxy`, made by the operator
```

| Process | Runs as | Owns | Interface |
| --- | --- | --- | --- |
| `legionctl-system` | root, system service (`--safe`: `legionctl-safe.service`) | machine state, firmware, power, GPU, display policy, kernel, eBPF, Fn keys | D-Bus `org.legionctl.System1`, polkit per method |
| `legionctl-operator` | user service (socket-activated) | operator database and its schema steps with backups, operation registry, approvals, agents, sessions and their sandboxes, tools, workspaces, config, OSD and Mutter calls when a graphical session exists | `$XDG_RUNTIME_DIR/legionctl/api.sock` (axum, HTTP + SSE) |
| `legionctl` | user | nothing | CLI, `legionctl mcp`, `legionctl mcp-proxy`, `legionctl hook <event>` |
| `legionctl-app` | user | nothing | GPUI |
| `legionctl-greeter` | user `greeter` (greetd, in cage) | nothing | GPUI; logins through greetd, machine actions through `legionctl-system` |

Why these boundaries:

- Privilege: the root binary links only machine code. No HTTP server, model client or package fetcher runs as root.
- Failure isolation: Fn keys and power policy keep working when the operator restarts. Agents keep their model
  calls when the operator restarts.
- Clients have one path: the operator API. When the operator is not running, a client says so and offers to start
  it. Clients never read the databases or sysfs directly.

## One operation registry

Each operation is declared once in `legionctl-api`: name, input and output schemas, permission class (read,
workspace, machine, account) and engine. The registry generates every surface:

| From `power.apply` | Surface |
| --- | --- |
| HTTP | `POST /v1/ops/power.apply` |
| MCP tool | `power_apply` |
| CLI | `legionctl power apply --source ac` |
| polkit action | `org.legionctl.power.apply` |
| event log kind | `power.apply` with actor, input and outcome |
| GPUI app | view models read the same schema |

A surface cannot have an operation that the others do not have.

## Crates

```
crates/
  legionctl-core      config layers (precedence, locks, explain), event log, one-writer databases and their schema steps,
                      dossiers, workspaces, sessions, threads, code graph and checks, agents, setup, rlcd, views
  legionctl-api       operation registry, schemas, API version, unix-socket client, CLI, render
  legionctl-machine   sysroot (paths over /sys, /proc, /dev, /run), sensors, firmware attributes, power policy and
                      configsets, cpufreq, power-profiles-daemon, GPU, display, fans, kernel, boot, greeter, windows, VFIO
  legionctl-system    the root daemon (bin legionctl-system) with its eBPF sensors (aya)
  legionctl-operator  the user operator (bin legionctl-operator): engine, gateway (19), sessions (ACP), threads, views,
                      resources, rlcd backends
  legionctl           the client (bin legionctl): CLI, MCP server and proxy, hook adapter
  legionctl-app       GPUI (bins legionctl-app and legionctl-greeter)
```

A module becomes its own crate only when its size or its dependencies require it. Until then it stays a module.

## Names and paths

| Kind | Name |
| --- | --- |
| binaries | `/usr/bin/legionctl`, `/usr/bin/legionctl-app`; `/usr/lib/legionctl/legionctl-{system,operator,greeter,install,static}` |
| system units | `legionctl-system.service`, `legionctl-safe.service` |
| user units | `legionctl-operator.service` and `.socket`, `legionctl-windows-vm.service`, `legionctl-session-<id>.scope` |
| D-Bus, polkit | `org.legionctl.System1`, actions `org.legionctl.<operation>` |
| config layers | built-in defaults (`config/defaults.jsonc`) → `/etc/legionctl` → `~/.config/legionctl` → `<repo>/.legionctl` → session; JSONC (14) |
| state | `/var/lib/legionctl/system.db`, `~/.local/state/legionctl/operator.db`, `~/.local/share/legionctl/{store,sessions}`, `~/.cache/legionctl/{gateway,tools}` |
| runtime | `/run/legionctl/`, `$XDG_RUNTIME_DIR/legionctl/`, `/sys/fs/bpf/legionctl/` |
| udev, hwdb | `99-legionctl.rules`, `90-legionctl-keys.hwdb` |
| GNOME extension | `legionctl-osd@legionctl` |
| agents | plugin `legionctl` (`/legionctl:status`, `/legionctl:skills`, ...), MCP server `legionctl`, env `LEGIONCTL_*` |
| worktrees | branch `legionctl/<name>` |

## Command tree

The groups of `legionctl --help` (2026-10-16; todo-78 regroups them):

| Area | Commands | Engine |
| --- | --- | --- |
| overview | `status`, `load`, `doctor`, `capabilities`, `validate`, `events`, `trace`, `monitor`, `sensors`, `undo` | operator |
| power and thermal | `power apply`, `plan`, `knobs`, `rules`, `fan`, `battery`, `bench`, `tune`, `record`, `replay`, `cleanup` | system, operator plans |
| graphics and display | `gpu`, `display`, `overlay`, `rgb`, `vfio`, `run` | system, operator for Mutter |
| kernel, boot and firmware | `kernel`, `boot`, `greeter`, `firmware`, `windows` | system |
| virtualization | `virt`, `images`, `resources` | operator, system for kernel parts |
| config | `config` (show, explain, layers) | operator |
| agents | `agents`, `setup`, `skills`, `reminders`, `hooks`, `tools`, `packages`, `cache`, `gateway`, `rlcd` | operator |
| work | `dossier`, `workspace`, `code`, `session`, `thread`, `views`, `jobs` | operator |
| surfaces | `mcp`, `mcp-proxy`, `hook` | client |

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
