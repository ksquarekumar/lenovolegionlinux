---
name: workspace-practice
description: How to change code with legionctl - the dossier, the dossier's workspace (a git worktree), topic workspaces for parallel agents, checkpoint, merge, close, and the hooks and nudges that keep it in order. Use before the first code change in a repository, when you start parallel agents on one dossier, and before you commit, merge or close.
user-invocable: true
---

# Workspace practice

Change code only in a legionctl workspace. Never change files in the repository's main checkout. Keep the record
of the work in the dossier (skill `legionctl:dossier-practice`).

## The two tools

| Tool | What it is | Where |
| --- | --- | --- |
| Dossier | Your own space for one scope: `plan.md`, MEMORY, DESIGN, EVIDENCE, SPIKE. Never part of the product. | `~/.agentworkspace/<dossier>/` |
| Workspace | A git worktree of one repository for one dossier, with the dossier linked in as `.agentworkspace`. | `<workspace.root>/<repo>/<dossier>` on branch `legionctl/<dossier>` |
| Topic workspace | One more worktree of the same dossier, for one agent that works in parallel with others. | `<workspace.root>/<repo>/<dossier>--<topic>` on branch `legionctl/<dossier>--<topic>` |

legionctl does the git work around you. You edit files and ask for a checkpoint. The operator (`legionctl-operator`,
a user service on `$XDG_RUNTIME_DIR/legionctl/api.sock`) runs each command as you. No command here needs root.

## Commands

Each command is also an MCP tool: replace the spaces with `_` (`workspace checkpoint` is `workspace_checkpoint`).

| Command | What it does | Permission class |
| --- | --- | --- |
| `dossier use <name>` | Binds this session to the dossier. A new name makes a new dossier. `--steal` takes it from another live session (the user must agree). | session |
| `dossier active`, `dossier locks` | Shows this session's dossier; shows which session holds which dossier. | read |
| `workspace open` | Makes (or opens again) the dossier's workspace. `--repo <folder>` names the repository (default `.`). `--base <ref>` sets the base (default: the main checkout's branch). | session |
| `workspace open --topic <t>` | Makes (or opens again) a topic workspace. The first open starts from the base; a reopen continues the same branch. | session |
| `workspace status` | Ahead and behind the base, staged, unstaged, untracked, conflicts, and drift from the record. | read |
| `workspace list` | Every open workspace of every dossier, topic workspaces too. | read |
| `workspace checkpoint "<message>"` | Stages everything in the workspace and commits it. | session |
| `workspace merge` | Rebases the branch onto the base, then fast-forwards the base in the main checkout. The main checkout must be clean. | workspace |
| `workspace close` | Removes the worktree. The branch and the dossier stay. It refuses uncommitted changes; `--force` discards them. | session |

Permission classes: `read` and `session` calls run at once. A `workspace` call from an agent waits for the user: a
desktop notification with Allow and Deny (120 s). No answer is a refusal. So an agent's `workspace merge` always
asks the user.

### Which workspace a command acts on

`status`, `checkpoint`, `merge` and `close` act on:

1. the workspace that contains the working directory (also from a subfolder), else
2. the dossier's own workspace in that repository, or
3. the topic workspace that `--topic <t>` names, from anywhere in the repository.

CAUTION: Agents that share one MCP server must give `topic` (or `repo` as the workspace path) to each workspace
tool. The MCP server's working directory is the main checkout, not your topic workspace. Without `topic`, the
tool acts on the dossier's own workspace.

A folder that is not in a git repository gets a folder workspace (an overlay layer, no branch). `--topic` is
refused there.

## What the hooks do

The plugin's hooks (`hooks/hooks.json`) run `legionctl hook <event>` in each Claude Code session:

| Event | What it does |
| --- | --- |
| `session-start` | Binds the session to the last dossier used in this place, when it is free (`dossier.auto_bind`). Tells you the dossier and the workspace (with drift), or tells you to open one. |
| `prompt` | Blocks each prompt while the session has no dossier (`dossier.gate`). Only `/legionctl:dossier` passes. Adds the reminders. |
| `pre-tool` | Denies `Write`, `Edit`, `MultiEdit` and `NotebookEdit` into a dossier that is not this session's. |
| `stop` | Counts the replies that changed the workspace without a dossier write. At 5 replies (`dossier.nudge_turns`) you get a note (info), at 10 one reply is stopped (warn), at 15 each reply is stopped (block). A write into your dossier resets the count. It also tells you when the main checkout has changes and the dossier has no workspace there (`workspace.nudge`). |
| `session-end` | Releases the dossier binding. |

## How to track the work

- `workspace status`: the git state and drift of one workspace.
- `workspace list`: all workspaces and their branches.
- `events`: each call that is not a read, with the actor and the result (kind = the operation name, for example
  `workspace.checkpoint`). `trace` follows the log live.
- `dossier nudges [--of <session>]`: each nudge level and each answer.
- `dossier check` and `dossier graph`: ledger findings and the links between plan rows and artifacts.
- In the dossier: write the commit SHA and its scope in the `plan.md` row in the same turn.

## Rules

1. Bind a dossier before the first change: `dossier use <name>`.
2. Open the workspace before the first change. Work in the `path` that `workspace open` returns.
3. Do not edit, stage or commit in the main checkout. If you did, tell the user and move the change to the
   workspace.
4. Commit with `workspace checkpoint`, not raw `git commit`. Use the message of the plan entry.
5. Record each checkpoint (SHA, what it does, tests) in the dossier in the same turn.
6. Ask the user before `workspace merge`. If a lead agent merges, do not merge yourself.
7. Do not push. A push costs CI minutes; the user or the lead pushes tested batches (skill
   `legionctl:verify-and-ship`).
8. If `workspace status` shows drift, say what happened and propose the fix (switch the branch back, or open the
   workspace again).

## Scenario: one agent, one change

1. `legionctl dossier use legionctl`
2. `legionctl workspace open --repo /data/git/lenovolegionlinux`
3. Edit files in the returned path. Run the checks (skill `legionctl:verify-and-ship`).
4. `legionctl workspace checkpoint "Kernels: list, default and try through the boot takeover"`
5. Write the SHA into the `plan.md` row with `dossier edit`.
6. Ask the user. Then `legionctl workspace merge`.
7. `legionctl workspace close` when the work in this repository is complete.

## Scenario: a lead and two parallel agents

1. The lead binds the dossier and gives each agent a topic: `kernels` and `display`.
2. Each agent runs `legionctl workspace open --topic kernels` (or `--topic display`) and works in its own path.
3. Each agent commits from inside its path: `legionctl workspace checkpoint "<message>"`. With MCP tools, each
   call gets `topic`.
4. The lead merges one topic at a time: `legionctl workspace merge --topic kernels`, then
   `legionctl workspace merge --topic display`. The second merge rebases onto the new base, so the history stays
   linear.
5. On a conflict, resolve it in the topic workspace path, run `git rebase --continue`, and merge again.
6. The lead closes each topic: `legionctl workspace close --topic kernels`. The branch stays.
