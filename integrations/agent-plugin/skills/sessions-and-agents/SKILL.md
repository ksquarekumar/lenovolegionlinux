---
name: sessions-and-agents
description: How to run a coding agent in a legionctl session (a sandbox on the host, a container or a VM, with the git gate, its own dossier and workspace, optional GPU, an account profile and the model gateway) instead of a plain subagent, and how to follow it - session start, run, list, activity, pause, unpause, resume, stop, jobs, tools calls, agents accounts and usage, gateway usage. Use when you plan to give work to another agent, when a task must run headless or isolated, and when you check what a session did.
user-invocable: true
---

# Sessions and agents

## What a legionctl session is

A session is one agent (Claude Code or Codex) that legionctl starts for a dossier, in a sandbox, in the dossier's
workspace (skill `legionctl:workspace-practice`). The operator records the session, gives it its own token, and
keeps its calls and its programs in the event log.

| Where (`--in`) | What runs | What it needs |
| --- | --- | --- |
| `host` (default) | A systemd user scope (`legionctl-session-<id>`), then bwrap: `/` and your home read-only, writable only the workspace, the dossier, the repository's `.git`, the runtime folder and `session.writable`. | bwrap |
| `container` | `docker run` of the repository's agent image, with the workspace, `.git` and the dossier mounted at their own paths and a private home. | `legionctl workspace pack` first |
| `vm` | QEMU on a throwaway layer over the repository's VM disk; the folders are shared with virtiofs. | `legionctl workspace pack --as vm` first |

In each place:

- The git gate: `git` in the sandbox is legionctl. Local commands run. Push, fetch, pull, remote, `reset --hard`,
  rebase, `clean -f`, `branch -D` and other history or remote commands are refused (`gates.git.deny`).
- The MCP servers come through `legionctl mcp-proxy`: only the allowed tools, every call logged.
- `--gpu` gives the NVIDIA GPU for this run only: the device nodes on the host and in a container; the whole GPU in
  a VM (VFIO, a polkit dialog, refused while the desktop uses the GPU; the disk needs `workspace pack --as vm --gpu`).
- `--account <profile>` uses an account profile (`agents accounts add`): its own logins and rate limits.
- The model gateway (when `gateway.port` is set) counts tokens and money per account and model, and enforces
  `gateway.budgets` (`day_usd`, `month_usd`). The gateway works inside containers and VMs too.

## Session or plain subagent

| Use a legionctl session when | Use a plain subagent when |
| --- | --- |
| The agent must not reach your home, the remotes or other sockets (sandbox, git gate). | The task is short, reads only, or needs this conversation's context. |
| The run is long or headless and must continue after a restart (a job). | You wait for the answer in this turn. |
| It needs another account, a budget, a container or VM image, or the GPU in isolation. | It uses the same account and the host tools. |
| You must audit later what it ran (`session activity`, `tools calls`). | The transcript is enough. |

A plain subagent that changes code still uses its own topic workspace (`workspace open --topic <t>`).

## Commands

| Command | What it does | Class |
| --- | --- | --- |
| `session run <agent> "<prompt>"` | Runs the agent headless through its ACP adapter, in the same sandbox, gates and MCP servers. Returns the answer. A job. Options: `--dossier`, `--repo`, `--account`, `--in host\|container\|vm`, `--gpu`, `--steal`, `--tag`. | workspace |
| `session start <agent>` | Prepares an interactive session; the `legionctl` client then runs it in your terminal. Same options as `run`, without `--tag`. | workspace |
| `session list` | Sessions, newest first, with the state of their scope, container or VM. | read |
| `session activity <id>` | The programs the session's processes started (eBPF sensor of `legionctl-system`). | read |
| `session pause <id>`, `session unpause <id>` | Freezes and thaws the session (cgroup freezer, or the container). Memory stays. | workspace |
| `session stop <id>` | Stops the session and everything it started. | workspace |
| `session resume <id> [--account <profile>]` | Continues an ended session: same id, dossier and workspace, the agent's resume command. | workspace |
| `workspace pack [--as container\|vm] [--gpu] [--service <s>]` | Makes the repository's agent image or VM disk. A job. | workspace |
| `jobs list`, `jobs show <id>` | The long operations (`session run`, `images build`, `workspace pack`) and how each ended. The operator resumes a job that a restart cut off (3 attempts). | read |
| `tools calls [--session <id>]` | The MCP tool calls of session agents. | read |
| `agents accounts list`, `agents accounts add <name>` | Account profiles and their logins. Logins rest in the keyring. | read, workspace |
| `agents usage [--days <n>] [--account <profile>]` | Tokens per agent, day and model from the agents' own transcripts. | read |
| `gateway usage` | Each call through the gateway: account, model, tokens, status, time. | read |
| `agents list`, `agents set`, `agents plugins list\|install` | The agents' settings files and plugins. | read, workspace |

An agent's `workspace`-class call waits for the user (a desktop notification, Allow or Deny, 120 s). So an agent
cannot start, pause or stop a session without the user.

## How to follow a session

1. `legionctl session list`: the id and the state.
2. `legionctl session run` writes the answer while it runs. From another terminal, `legionctl trace` shows the
   `session.output` events live.
3. `legionctl jobs list`, then `legionctl jobs show <id>`: the result, or why it failed.
4. `legionctl session activity <id>`: what it executed.
5. `legionctl tools calls --session <id>`: the MCP calls it made.
6. `legionctl workspace status --topic <t>` or `workspace list`: its commits and changes.
7. `legionctl dossier nudges --of <id>`: whether it kept its dossier current.
8. `legionctl gateway usage` and `legionctl agents usage`: tokens and cost.

`--json` gives the result only at the end, without the live output.

## Rules

1. Check `legionctl load` before you start parallel runs.
2. Give each parallel agent its own topic workspace and tell it the topic.
3. Do not use `--gpu` live without the user's agreement (rule: containers without GPU only).
4. Do not use `--steal` without the user's agreement.
5. Do not return a secret (a token, a key, a login file) in an answer or a log.
6. Record each session id, its job id and its result in the dossier.

## Scenario: a headless review in a container

1. Once per repository: `legionctl workspace pack`. Follow it with `legionctl jobs list`.
2. `legionctl session run claude "Review the last commit for bugs" --in container --dossier legionctl`.
3. Read the answer. Check `legionctl jobs show <id>` and `legionctl tools calls --session <id>`.
4. Write the session id and the findings into the dossier.

## Scenario: a long interactive session on a second account

1. `legionctl agents accounts add work`, then log in to the agent in that profile.
2. `legionctl session start claude --account work --dossier legionctl`.
3. If the rate limit stops it: `legionctl session resume <id> --account <other profile>`. The conversation goes on.
