# Use legionctl

This guide tells you how to install legionctl, use the app, and give work to agents safely. Each
command here is on main. A feature that is not on main has the label "planned (v1)" and its plan row.

| Section | You want to |
| --- | --- |
| [Install](#install) | install the package, the services and the agent setup |
| [The app](#the-app) | see the machine and talk to an agent |
| [Commands by task](#commands-by-task) | find the command for a task |
| [Dossiers and workspaces](#dossiers-and-workspaces) | keep an agent's work and memory |
| [Threads and observers](#threads-and-observers) | let agents check each other |
| [rlcd](#rlcd) | fast typed decisions for observers |
| [Sessions and sandboxes](#sessions-and-sandboxes) | run an agent isolated or headless |
| [Greeter and boot entry](#greeter-and-boot-entry) | the login screen and the systemd-boot entry |
| [Safety](#safety) | know what needs you |

## Install

1. Build and install the Arch package. The first time, sudo asks for your password.

   ```bash
   make install-package
   ```

2. Start the root daemon and the scheduler loader.

   ```bash
   sudo systemctl enable --now legionctl-system.service scx_loader.service
   ```

3. Start your operator. systemd starts it again at the first call after a stop.

   ```bash
   systemctl --user enable --now legionctl-operator.socket legionctl-operator.service
   ```

4. Enable the OSD. It shows at the next login.

   ```bash
   gnome-extensions enable legionctl-osd@legionctl
   ```

5. Set up the agents. This writes the MCP server, the skills and the agents for Claude Code and Codex, and installs
   the ACP adapters (`claude-code-acp`, `codex-acp`) with bun. It changes only your user files, with backups.

   ```bash
   legionctl setup apply
   legionctl setup check     # each line "current", then "all set up"
   ```

6. In Claude Code, add the plugin: `/plugin marketplace add /data/git/lenovolegionlinux`, then
   `/plugin install legionctl@lenovolegionlinux`.

7. Run `legionctl doctor`. Each finding names the command that fixes it.

Optional packages enable more parts (`pacman -Qi legionctl` lists them): `greetd` and `cage` for the greeter,
`systemd-ukify` and `sbsigntools` for the boot entry, `docker` or `qemu-base` for sandboxed sessions, `stress-ng`
for bench and tune.

## The app

`legionctl-app [<view>]` opens the desktop app. It needs the operator.

| Part | What |
| --- | --- |
| Pane 1 | the pages: Now, Apps & GPU, Health, Bench, Resources |
| Pane 2 | the navigator: in Work, New chat and your chats (your threads), newest first |
| Pane 3 | the page, or a view that an agent or you placed (`legionctl views place`) |
| Chat bar | `/` lists the commands; in a chat, other text is a message to the agent, and the answer comes into the tab |

A chat keeps its conversation in a dossier. Choose one with `/dossier use <name>`, or make one with `/dossier create
<name>` (the title bar shows it; a click opens the Dossiers view).

Pick the agent of the chat bar with `/agent claude` or `/agent codex` (the names of config `session.acp`). The pick
is a row of the thread's log, so every app window on the thread uses the same agent. With no pick, config `app.agent` applies
(default `claude`).

![Bench page: CPU score over time and the runs](../media/app-bench.webp)

Planned (v1, plan#todo-146, DESIGN-09): a 56 px rail with the spaces Machine, Work and Agents plus Activity, a
navigator pane, tabs, and every screen as a view that an agent can remake. Step G1 is built on the topic branch
`gen-ui` and is not merged.

## Commands by task

`legionctl --help` lists every command group. The table of groups, with the permission class and the skill of each,
is [reference-command-groups.md](../../integrations/agent-plugin/skills/tool-lifecycle/refs/reference-command-groups.md).

| Task | Commands |
| --- | --- |
| Look at the machine | `status`, `sensors`, `monitor`, `load`, `doctor`, `events`, `trace` |
| Power policy | `plan`, `knobs list`, `knobs show <knob>`, `rules list`, `rules enable <layer>`, `power apply`, `undo <event>` |
| Fans and battery | `fan status`, `fan max`, `battery status`, `battery mode` |
| GPU and display | `gpu status`, `gpu primary`, `gpu give`, `display status`, `display fix <id>`, `run --gpu nvidia -- <cmd>` |
| Kernels and boot | `kernel list`, `kernel try`, `kernel cmdline set`, `kernel bench`, `kernel build`, `boot status` |
| Windows | `windows status`, `windows reboot`, `windows vm start`, `windows disk attach` |
| Measure and record | `bench run`, `bench compare`, `tune`, `record <dir>`, `replay <dir>` |
| Resources | `resources list`, `resources act` (VMs, GPU, mounts, configsets, sessions, workspaces) |

## Dossiers and workspaces

A dossier is the memory of one piece of work, in `~/agentWorkspaces/<name>/`: `plan.md`, numbered `MEMORY`,
`DESIGN`, `EVIDENCE`, `SPIKE` and `JOURNAL` files. Each agent session binds one dossier; the hooks block prompts until
it has one.

```bash
legionctl dossier list
legionctl dossier read legionctl/plan.md
legionctl dossier check                 # missing plan sections, links to nothing
```

A workspace is a git worktree for a dossier (branch `legionctl/<dossier>`) or for a topic
(`legionctl/<dossier>--<topic>`), so parallel agents do not change the main checkout.

```bash
legionctl workspace open --topic docs-refresh
legionctl workspace checkpoint --topic docs-refresh "message"   # runs the gate: checks and the test review
legionctl workspace merge --topic docs-refresh                    # rebase, gate, fast-forward
```

The code graph shows what a branch changed (`legionctl code graph changes`); the planned graph in `planning/`
compares planned and built work (`legionctl code plan status`). The rules: skills `dossier-practice` and
`workspace-practice`.

## Threads and observers

A thread is one task. `main0` owns it; observers watch its events, review its tests and remind it of skipped steps.

```bash
legionctl thread start "Fix the fan page" --team vanilla   # main0, thread-observer, ui-composer
legionctl thread show 1
legionctl thread peers                                    # who else works on this dossier, and their state
```

An observer sends a `note`, `reminder` or `result`; a `hard-block` holds the main agent's tool until it acks
(`thread ack <id>`). The outbox is in the operator's database, so messages survive restarts and compactions. The
full list: [reference-threads.md](../../integrations/agent-plugin/skills/sessions-and-agents/refs/reference-threads.md).

## rlcd

rlcd is a decision tier for agents: a hosted "System One" model answers typed questions (`choice`, `yesno`, `score`)
on a state. It gives no text, only the typed answers. The vendors give 70-500 ms and a low price per call.

```bash
legionctl rlcd ask --state "tests failed after the last edit" 'next:choice:ignore,note,escalate'
legionctl rlcd decisions     # each decision with its later outcome
```

The backends are in config `rlcd.backends`, with two wire shapes: `system-one` (OpenRouter) and `openai`. The key
is in the keyring. The state leaves the machine with its secrets removed. A team member with
`route: ["rlcd", "acp"]` wakes its agent only when rlcd escalates. A failed rlcd call fails; the next step does not
run in its place.

Planned (v1): the `api` route step and rlcd on hot paths such as gateway model choice and the chat bar
(plan#todo-139).

## Sessions and sandboxes

A session is one agent (Claude Code or Codex) that legionctl starts in a sandbox, in the dossier's workspace.

```bash
legionctl session start claude                       # interactive, in this terminal
legionctl session run claude "fix the failing test"  # headless, through ACP; returns the answer
legionctl session list
```

| Option | What |
| --- | --- |
| `--in host` | bwrap: your home read-only, the workspace writable |
| `--in container` | docker; run `legionctl workspace pack` first |
| `--in vm` | QEMU; run `legionctl workspace pack --as vm` first |
| `--gpu` | the NVIDIA GPU for this run only |
| `--account <profile>` | another account profile, with its own logins and limits |

In each sandbox `git` is the git gate: local commands run; push, fetch, `reset --hard`, rebase and other history or
remote commands are refused. MCP servers come through `legionctl mcp-proxy`, which logs each call. When
`gateway.port` is set, the model gateway counts tokens and cost and enforces budgets. Details: skill
`sessions-and-agents`.

Planned (v1, plan#todo-153): environments with no network except through the gateway, declared connections, and
snapshots with write-back and undo.

## Greeter and boot entry

The greeter is legionctl's login screen under greetd, in place of GDM. GDM stays installed and disabled.

```bash
legionctl greeter status
legionctl greeter install --dry-run
legionctl greeter install        # at the next start; legionctl greeter remove brings GDM back
```

It lists the desktops of `/usr/share/wayland-sessions` and comes back after you log out. Before login, it can start
or stop the Windows VM, attach the Windows disk and set the desktop GPU, each through the root daemon. Planned (v1,
plan#todo-150): it remembers your last desktop.

The boot entry is systemd-boot with a signed kernel image per kernel, started through a copy of shim, beside the
entry you have now. `kernel default` and `kernel try` need it.

```bash
legionctl boot status
legionctl boot install --dry-run
legionctl boot install           # then restart, then: legionctl boot confirm
```

Caution: install `systemd-ukify` and `sbsigntools` before `boot install`. On main, a missing tool stops the install
at its step (plan#todo-149; the fix is built on the topic branch `v1-bugs`).

## Safety

| Class | Who approves | Examples |
| --- | --- | --- |
| `read` | nobody; it changes nothing | `status`, `doctor`, `plan`, `dossier read` |
| `session` | nobody; only the caller's own session | `dossier write`, `workspace checkpoint` |
| `workspace` | you, for an agent: a desktop notification, 120 s | `session start`, `workspace merge`, `setup apply` |
| `machine` | you, in a polkit dialog; an agent cannot | `power apply`, `kernel try`, `boot install`, `fan max` |

The rules the user set for agents:

- Do not guess ACPI or WMI write calls on the real firmware.
- Do not run `windows reboot`, tune sweeps or polkit writes live.
- Do not pause the user's WinBoat VM.
- Never return secrets.
- Make only safe live writes: read-only checks, recorded runs, containers without a GPU, short stress runs.

What stays with you: each polkit dialog, merges of `.legionctl/` changes, the first `boot install`, fan curve writes
(the WMI calls are not verified, plan#todo-17), and the first live use of a GPU in a VM.
