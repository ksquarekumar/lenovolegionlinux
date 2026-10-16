---
name: sessions-and-agents
description: How to run a coding agent in a legionctl session (a sandbox on the host, in a container or a VM, with the git gate, its own dossier and workspace, an optional GPU, an account profile and the model gateway) instead of a plain subagent, and how to follow it. Use when you plan to give work to another agent, when a task must run headless or isolated, and when you check what a session did.
user-invocable: true
---

# Sessions and agents

1. A session is one agent (Claude Code or Codex) that legionctl starts for a dossier, in a sandbox, in the dossier's
   workspace. The operator gives it its own token and logs its calls and programs.
2. Use a session when the agent must not reach your home, the remotes or other sockets, when the run is long or
   headless, when it needs another account, a budget, an image or an isolated GPU, or when you must audit it later.
3. Use a plain subagent when the task is short, reads only, needs this conversation's context, or you wait for the
   answer in this turn. A subagent that changes code still uses its own topic workspace (skill workspace-practice).
4. `session run <agent> "<prompt>"` runs the agent headless through its ACP adapter and returns its answer. It is a job.
   `--acp <id>` goes on in the ACP session of an earlier run (its `acp`); the run fails when the agent cannot load
   sessions. `session commands <session>` lists the commands that the agent offered in that run; a prompt that
   starts with `/<name>` runs one.
5. `session start <agent>` prepares an interactive session; the client then runs it in your terminal. It takes the
   options of `run` except `--tag`.
6. `--in host|container|vm` picks the sandbox. A container needs `workspace pack` first; a VM needs `workspace pack --as
   vm` first.
7. In each sandbox, `git` is the git gate: local commands run; push, fetch, pull, remote, `reset --hard`, rebase, `clean
   -f`, `branch -D` and other history or remote commands are refused (`gates.git.deny`).
8. MCP servers come through `legionctl mcp-proxy`: only the allowed tools, and each call is logged.
9. `--gpu` gives the NVIDIA GPU for this run only. In a VM it needs a polkit dialog, a disk from `workspace pack --as vm
   --gpu`, and a desktop that does not use the GPU. See skill machine-tools for the live rule.
10. `--account <profile>` uses an account profile (`agents accounts add <name>`) with its own logins and rate limits.
    Logins stay in the keyring.
11. When `gateway.port` is set, the model gateway counts tokens and cost per account and model, and enforces
    `gateway.budgets` (`day_usd`, `month_usd`), also in containers and VMs.
12. `session pause` and `session unpause` freeze and thaw a session; its memory stays. `session stop` stops it and
    everything it started.
13. `session resume <id> [--account <profile>]` continues an ended session with the same id, dossier and workspace.
14. These commands are class workspace, so an agent cannot start, pause or stop a session without the user (skill
    tool-lifecycle).
15. The operator resumes a job that a restart cut off, three times at most.
16. Give each parallel agent its own topic and tell it the topic. Check `legionctl load` first.
17. Do not use `--steal` without the user's agreement.
18. Record each session id, its job id and its result in the dossier.
19. Before you start or message a parallel agent, run `thread peers`: it shows who works on your dossier, workspace or
    thread, and its state.
20. A thread message in your prompt or at the end of a reply comes from a member of your thread. Act on it, then
    `thread ack <id>`. A hard-block holds its tool and each reply end until you ack it.
21. Send through the thread outbox (`thread send`); it is durable. For an urgent message to an idle Claude peer, also
    use `SendMessage`; the post-tool hook records it in the thread log.
22. A member is an archetype (`main-agent`, `thread-observer`, `ui-composer`) plus its config `agents.configs.<name>`.
    Before you start a team, run `agents check --team <team> --harness claude|codex`.
23. If `agents check` refuses a field, change the config. Do not drop the field in silence.
24. When you delegate work (a subagent, a session, a thread member), tell the delegate to read the full dossier plan
    (`dossier read <dossier>/plan.md`) and the MEMORY and DESIGN files its rows name by itself, not only your prompt.
    Tell a reviewer or auditor to be adversarial and thorough and to report what it did not check. When you are the
    delegate, read them yourself first.

## Refs

- `refs/reference-sandboxes.md` - read to choose `--in` and to know what each sandbox can reach.
- `refs/reference-follow-a-session.md` - read to follow or audit a session.
- `refs/scenario-headless-review.md` - read for a headless run in a container.
- `refs/scenario-second-account.md` - read when a rate limit stops a session.
- `refs/reference-threads.md` - read when you start a thread, add a member or a watch rule, or send a message.
- `refs/reference-agent-configs.md` - read when you write an agent config or a team file, or `agents check` refuses
  one.
