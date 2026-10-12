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
In a git operating directory, `.agentworkspace` goes into `.git/info/exclude` (local to the clone, not a commit), so
it never shows in `git status` and never reaches the product.

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
- Not known: the agent is told to start or choose a workspace and confirm the operating directory
  (`/legionctl:workspace`), and every tool that writes or runs commands is denied until then. Reading and the
  workspace commands stay allowed.

Claude Code enforces this with its `PreToolUse` hook (deny). For Codex and other agents, enforcement is the
session launch itself (14): the harness starts the agent only inside a confirmed operating directory.

## Sandboxed instances

Processes (bwrap), containers and VMs (17) get the same layout: the operating directory mounted read-write as the
working directory, the named workspace mounted at `<operating directory>/.agentworkspace`, nothing else writable.
A container or VM for a workspace is recorded in `workspace.db`, so it is reused, not recreated, when the workspace
is opened again.
