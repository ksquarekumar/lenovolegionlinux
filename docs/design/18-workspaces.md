# 18 — Workspaces: operating directory, named workspace, dossier

Requirements (2026-10-12): an agent works only after its workspace and operating directory are known; work is a
plannable set of changes toward objectives (Y1, Y2, ...) over typed entities (a repo, a folder of files for a
goal, ...), linked to a code graph; the operating directory is a copy-on-write copy, merged back with approval;
agent data (plans, evidence, scratch work) stays outside the product's git tree; sandboxed processes, containers
and VMs use the same two directories.

Replaces the "plan workspaces" section of 12-operator. Sessions (14) and sandboxes (14, 17) use these concepts.

## Two directories

| Concept | What it is | Where |
| --- | --- | --- |
| **Operating directory** | The directory the agent's working directory is pinned to: a copy-on-write copy of a source (a repo or a plain folder). The agent changes only this copy. | worktree or overlay (14-sessions, "Operating directory: copy-on-write") |
| **Named workspace** | The agent's own space for one piece of work: dossier, plans, evidence, scratch files. Never part of the product. | `~/.agentworkspace/<name>/` |

The named workspace appears in the operating directory's root as a symlink, `.agentworkspace → ~/.agentworkspace/<name>`.
In a git operating directory, `/.agentworkspace` is a committed line in the repo's `.gitignore` (decided
2026-10-12), so every clone and worktree ignores it. Workspace open adds the line if it is missing, as its own
commit on the base branch, after approval.

## `~/.agentworkspace`

```
~/.agentworkspace/
  workspace.db                 SQLite (WAL): workspaces, entities, objectives, changes, sessions, events
  codegraphs/<repo-id>.db      one ctx graph per unique repo (ctx graph index --db ...), updated after each change set
  <name>/
    dossier.toml               title, objectives Y1..Yn, entities, operating directories, status
    plan/                      change sets: what changes where, toward which objective, in which order
    evidence/                  proofs, prototypes, research, crawls
    verify/                    e2e, UI and eval results per objective
    scratch/                   anything else
    log.jsonl                  intent and decisions, read before each change set starts
```

- The folders under `<name>/` are a named, flexible pattern, not a fixed schema: the ones above are the default
  pattern, and a workspace can add its own (for example `homework/`, `prototypes/`, `sanity/`). `dossier.toml`
  names the pattern and what each folder is for, so tools and agents read it instead of assuming.
- `<repo-id>` comes from the repo's first commit hash, so clones and worktrees of one repo share one graph.
- Graphs never live inside a repo: ctx's default (`.graf/index.db` in an ancestor) is not used.

## Dossier model

| Type | Fields |
| --- | --- |
| Entity | type (`repo`, `folder`, `document`, `service`, ...), path or URL, notes, hooks, code graph (repos) |
| Objective | id (Y1, Y2, ...), outcome, how it is verified, status |
| Change set | objective, entity, scope (paths and symbols from the graph), dependencies, status, operating directory |
| Hook | event (`workspace.open`, `changeset.start`, `changeset.done`, `merge`), action (13 hooks manager) |

Ordering of change sets uses the code graph (12-operator, "Code graph and work ordering").

## Living work graph

`workspace.db` keeps the work itself as a graph: objectives, change sets, homework and sanity checks, evidence,
prototypes and tests are nodes; edges link them to each other and to code-graph symbols and paths. Each work item
also records how it runs: its entrypoints (binaries, services, scripts, test targets), the runtimes it needs
(toolchain versions, interpreters, containers or VMs from 17), and its dependencies and environment (packages,
services, environment variables, ports, devices such as the GPU). A sandbox (14, 17) or a packed workspace is
built from these records, and a change to a dependency marks the work items that use it for re-verification.
Every node has a
status (`planned`, `pending`, `in-progress`, `done`, `failed`, `abandoned`) with history, so an agent that resumes
work days later sees what was planned, what was done, what failed and why, and what the code graph says changed.

## Making the pattern obvious to agents

- At session start, the hook injects a short workspace brief: the workspace name, its folder pattern and what each
  folder is for, the open objectives, and the next pending nodes of the work graph.
- A `legionctl:workspace-practice` skill (reminder on by default, like ste100 and glanceable) says how to use the
  space: keep plans, homework, sanity checks, evidence, prototypes and tests in the workspace, never in the product
  tree; record each step's status in the work graph; read the log before a new change set.
- MCP tools give the agent the work graph directly: list and update nodes, link evidence to an objective, query
  which code-graph symbols a change set touches.

## Lifecycle

| Step | Git operating directory | Folder operating directory |
| --- | --- | --- |
| Open | worktree on `legionctl/<workspace>` from a recorded base ref; symlink and exclude entry | overlay copy; symlink |
| Work | the agent's pwd is pinned here; gates on unsafe git commands (14) | the agent's pwd is pinned here |
| Checkpoint | stage and commit per change set; graph update | snapshot of the overlay upper layer |
| Merge | with approval: rebase or merge into the base ref, conflicts shown as git state (14) | with approval: copy changed files back, shown as a diff first |
| Close | remove worktree; keep the branch until merged; workspace stays | remove overlay; workspace stays |

## Gate: no workspace, no work

At session start and on each prompt, the hook asks the operator for the session's workspace and operating
directory:

- Both known (from the session, or the current directory is an operating directory): the agent goes ahead, and
  the hook injects the dossier summary and the current change set.
- Not known: the prompt is blocked (decided 2026-10-12: everything, not only writes). The block message tells the
  user to open or choose a workspace and confirm the operating directory with `/legionctl:workspace`, which is
  the one command that passes the gate.

Claude Code enforces this with its `UserPromptSubmit` hook (block). For Codex and other agents, enforcement is the
session launch itself (14): the harness starts the agent only inside a confirmed operating directory.

## Sandboxed instances

Processes (bwrap), containers and VMs (17) get the same layout: the operating directory mounted read-write as the
working directory, the named workspace mounted at `<operating directory>/.agentworkspace`, nothing else writable.
A container or VM for a workspace is recorded in `workspace.db`, so it is reused, not recreated, when the workspace
is opened again.

A workspace can also be packaged and run as one sandboxed unit: `legionctl workspace pack <name> --as container|vm`
builds a copy-on-write image (an OCI image with the operating directory and the named workspace as layers, or a
qcow2 overlay over a base VM image) together with the session spec (agent, tools, skills, MCP servers, policy; 14).
`legionctl workspace run <name>` starts it with the same mounts, gates and accounting as a local session, and
changes come back only through the normal merge with approval. A packed workspace can move to another machine or
run several copies in parallel from one base.
