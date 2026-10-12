# 18 — Workspaces: operating directory and dossier

Requirements (2026-10-12): an agent works only after its session has an operating directory and a dossier; work is
a plannable set of changes toward objectives (Y1, Y2, ...) over typed entities (a repo, a folder of files for a
goal, ...), linked to a code graph; the operating directory is a copy-on-write copy, merged back with approval;
agent data (plans, memory, design, evidence, spikes) stays outside the product's git tree; sandboxed processes,
containers and VMs use the same two directories. legionctl (daemon, harness and plugin) takes care of worktrees,
copy-on-write, sandboxes and git; the agent only declares where it works and in which dossier.

The dossier pattern comes from the scope-dossier practice in `unity-services-monorepo-azure` (`AGENTS.md`,
`.agents/skills/references/dossier-artifacts.md`, the `safecoder dossier` command): artifact classes, routing
rules, one writable dossier per session, anchored edits. This note keeps that pattern and makes every artifact and
plan entry a node in a graph that links to objectives, evidence and code-graph symbols.

Replaces the "plan workspaces" section of 12-operator. Sessions (14) and sandboxes (14, 17) use these concepts.

## A session declares two things

| Declared | Default | What legionctl does with it |
| --- | --- | --- |
| **Operating directory** | the shell's working directory when the agent started | Resolves its source (a repo or a plain folder), makes or reuses the copy-on-write copy, pins the agent there, and reads and reconciles its git state |
| **Dossier name** | none; must be chosen or created | Makes `~/.agentworkspace/<dossier>/` this session's one writable dossier and links it into the operating directory |

## Gate: no dossier, no work

At session start and on each prompt, the plugin's hook asks the operator for the session's binding:

- The operating directory is always known (default: the shell's working directory). The brief shows it, with its
  source, copy and branch, so a wrong default is visible at once.
- With a dossier bound, the agent goes ahead and the hook injects the brief (below).
- Without a dossier, the prompt is blocked (decided 2026-10-12: everything, not only writes). The block message
  lists recent dossiers for this operating directory and the two ways forward: `/legionctl:dossier use <name>` or
  `/legionctl:dossier use <name> --create`. These are the only commands that pass the gate.
- Writes outside the operating directory and the dossier are denied (`PreToolUse`), so a session that started in
  the source repo cannot change the source by mistake.

Claude Code enforces this with its `UserPromptSubmit` and `PreToolUse` hooks. For Codex and other agents, the
harness enforces it at session launch (14): it starts the agent only with a binding.

## Two directories

| Concept | What it is | Where |
| --- | --- | --- |
| **Operating directory** | The agent's pinned working directory: a copy-on-write copy of the source. The agent changes only this copy. | git worktree or overlay (14-sessions) |
| **Dossier** | The agent's own space for one scope: plan, memory, design, evidence, spikes. Never part of the product. | `~/.agentworkspace/<dossier>/` |

The dossier appears in the operating directory's root as the symlink `.agentworkspace → ~/.agentworkspace/<dossier>`.
In a git operating directory, `/.agentworkspace` is a committed line in the repo's `.gitignore` (decided
2026-10-12). Workspace open adds the line if it is missing, as its own commit on the base branch, after approval.

## Git: legionctl owns the copy and its state

For a git source, legionctl does the git work around the agent; the agent edits files and asks for checkpoints:

| Operation | What legionctl does |
| --- | --- |
| open | A worktree from a recorded base ref on the dossier's active branch `legionctl/<dossier>` (created on first open, reused after). If the session started inside the source checkout, the agent is moved to the worktree path. |
| new branch | Another branch for the same dossier (`legionctl/<dossier>/<topic>`); it becomes the active branch. |
| checkpoint | Stage and commit the change set's files with a message from the plan entry; update the code graph. |
| merge | With approval: rebase or merge the active branch into the base ref; conflicts become git state (14) that the agent resolves in the worktree. |
| close | Remove the worktree; keep the branch until merged; the dossier stays. |

Reconciliation runs on every hook call and after every operation. It reads the git state (branch, base ref,
merge-base, ahead/behind, staged, unstaged, untracked, conflicted, resolved-but-not-committed; 14), compares it with
the recorded binding, and repairs or reports drift: a branch switched outside legionctl, a base ref that moved, a
worktree that was removed, a commit made by hand. Git commands that change history or remotes stay gated (14).

A plain-folder source gets an overlay copy; checkpoint snapshots the upper layer, and merge copies changed files
back after a diff and approval.

## `~/.agentworkspace`

```
~/.agentworkspace/
  dossier.toml                 global default: artifact classes, plan sections, routing rules, statuses, hooks
  workspace.db                 SQLite (WAL): bindings, locks, artifact index, work graph (graph tables, 13), events
  codegraphs/<repo-id>.db      one ctx graph per unique repo (ctx graph index --db ...), updated at each checkpoint
  <dossier>/
    dossier.toml               this dossier only: title, entities, objectives; may add artifact classes
    plan.md                    PLAN: the ledger
    MEMORY/NN-<topic>.md       working memory, numbered in creation order
    DESIGN/                    PRD.md, HLD.md, LLD.md or scope-appropriate names
    EVIDENCE/<class>/.../NN-<scope>.md
    SPIKE/                     disposable prototypes and reproducible experiments
    <class>/.../NN-<scope>.md  any other class the dossier declares
```

- `<repo-id>` comes from the repo's first commit hash, so clones and worktrees of one repo share one graph. Graphs
  never live inside a repo: ctx's default (`.graf/index.db` in an ancestor) is not used.
- The global `dossier.toml` is created on first use from the package default. A dossier's own `dossier.toml` holds
  its data and may add classes; the canonical classes cannot be removed or renamed.

## Artifact classes and routing

| Class | Path | Holds |
| --- | --- | --- |
| PLAN | `plan.md` | The ledger: state only, short entries that point to the artifact with the detail. Exists from open, may start almost empty. |
| MEMORY | `MEMORY/NN-<topic>.md` | Intake notes, source reads, decision and ambiguity registers, context moved from chat: the resumable core, not a transcript. Usually the first artifact. |
| DESIGN | `DESIGN/...` | Intended design, separate from plan status and from evidence; written after its inputs are settled in MEMORY. |
| EVIDENCE | `EVIDENCE/<class>/.../NN-<scope>.md` | Validation logs, reports, inventories, comparison tables, dumps; written when the check runs, from its real output. Every claim and finding has one. |
| SPIKE | `SPIKE/...` | Minimal, self-contained, disposable prototypes and experiments. Product code enters the product only through an approved change. |
| other | `<class>/.../NN-<scope>.md` | Declared in a `dossier.toml`, for example `HOMEWORK` or `PROBES`. |

Routing rules (checked by `legionctl dossier check`):

1. No plan content yet: write a MEMORY document.
2. `plan.md` records state; an entry that grows past a few sentences moves to MEMORY or EVIDENCE with a pointer.
3. Load-bearing chat context goes into MEMORY in the same turn.
4. Design content lives in DESIGN; the plan's `Design` section points to it.
5. Validation results live in EVIDENCE; the plan summarizes their state.
6. Reference, do not duplicate: point to artifacts and repository files by path.

## Linking: every artifact and ledger row is a graph node

Each artifact starts with a small front matter block:

```markdown
---
id: EV-validation-03
class: EVIDENCE/validation
status: done
objectives: [Y1]
entities: [lenovolegionlinux]
code: [crates/legionctl-machine/src/policy.rs#Machine::follow, crates/legionctl/tests/machine.rs]
supports: [plan#acceptance-2]
supersedes: [EV-validation-01]
---
```

`plan.md` keeps the required sections of the scope-dossier practice (`Purpose`, `Requested Outcome`, `Scope Fence`,
`Governing Principles`, `Instruction Inventory`, `Current Behavior / Findings`, `Open Questions / Decision Points`,
`Complexity / Trade-offs`, `Options / Recommendation`, `User Decisions / Approval`, `Design`, `Acceptance Criteria`,
`Requirement-to-Change Map`, `Done`, `ToDo`, `Deferred`, `Rejected`, `Validation / Evidence Status`). Sections that
are lists or maps (`Acceptance Criteria`, `Requirement-to-Change Map`, `ToDo`, `Done`, decisions) are written as
tables whose first column is a row id (`acceptance-2`, `rcm-4`), so each row is addressable.

legionctl indexes the front matter and the ledger rows into the work graph (graph tables in `workspace.db`):

- Nodes: objectives, plan rows, artifacts, change sets, commits, packages, entrypoints, runtimes.
- Edges: `supports`, `supersedes`, `implements` (plan row → change set → commits), `touches` (change set →
  code-graph symbols and paths, resolved against `codegraphs/<repo-id>.db` by `ATTACH`), `verified-by`
  (acceptance row → EVIDENCE), `depends-on`, `runs-with` (work item → entrypoint, runtime, environment).

`legionctl dossier check` then reports what the practice asks for as concrete findings: missing plan sections, a
claim or a `Done` row without EVIDENCE, an acceptance row with no `verified-by`, a dangling link, a DESIGN that
contradicts an approved decision, and EVIDENCE whose linked symbols changed after it was written (stale evidence).

## One writable dossier per session

| Command | Effect |
| --- | --- |
| `legionctl dossier use <name> [--create]` | Bind this session to the dossier (create it from the global pattern if asked). The previous one is released. |
| `legionctl dossier active`, `release`, `locks` | Show the binding; give it up; show which session holds which dossier and stale locks. |
| `legionctl dossier ls`, `read <path>`, `search <pattern>` | Read any dossier; lines print as `<line>:<hash>\|<text>`. |
| `legionctl dossier add <class> <slug>` | Create the next numbered artifact from stdin (`MEMORY/03-<slug>.md`); safe for parallel agents. |
| `legionctl dossier edit <path> replace\|insert-after\|delete <line:hash>` | Change anchored lines; an anchor from an outdated read is refused. |
| `legionctl dossier check`, `graph` | Routing and ledger findings; the work graph around a node. |

- Writes go only to the session's active dossier; reads work on every dossier. Every write is atomic.
- A dossier held by another live session is refused with that session's name; `--steal` takes it with the user's
  agreement. A lock idle for 12 hours is stale and taken over with a notice. Session end releases the lock.
- Paths outside `~/.agentworkspace` are refused.
- The same operations are MCP tools, so Codex and other agents use them without the shell.

## Living work graph

Every node has a status (`planned`, `pending`, `in-progress`, `done`, `failed`, `abandoned`) with history, so an
agent that resumes days later sees what was planned, done, failed and why, and what the code graph says changed.
Work items record how they run: entrypoints (binaries, services, scripts, test targets), runtimes (toolchain
versions, interpreters, containers or VMs from 17), dependencies and environment (packages, services, environment
variables, ports, devices such as the GPU). Provenance links them to the commits each change set produced, the
package versions and lock files in use, and changelog entries. From these records legionctl can rebuild a runnable
version of any recorded state (a container or VM image with the exact commits, packages and environment), and a
change to a dependency marks the work items that use it for re-verification.

## Making the pattern obvious to agents

- The brief, injected at session start and whenever the binding changes: session id, operating directory with its
  source, branch and git state, the dossier with its classes and routing rules, open objectives, the next pending
  ledger rows, and `dossier check` findings.
- A `legionctl:dossier-practice` skill (reminder on by default, like ste100 and glanceable) carries the routing
  rules and the ledger rules: keep requested, proposed, approved, implemented and validated state distinct; never
  describe a proposal as a decision; update the dossier in the same turn as the finding.
- MCP tools expose the dossier commands and the work graph.

## Example `dossier.toml`

`~/.agentworkspace/dossier.toml` (global default):

```toml
pattern = "scope-dossier"
statuses = ["planned", "pending", "in-progress", "done", "failed", "abandoned"]
plan_sections = [
  "Purpose", "Requested Outcome", "Scope Fence", "Governing Principles", "Instruction Inventory",
  "Current Behavior / Findings", "Open Questions / Decision Points", "Complexity / Trade-offs",
  "Options / Recommendation", "User Decisions / Approval", "Design", "Acceptance Criteria",
  "Requirement-to-Change Map", "Done", "ToDo", "Deferred", "Rejected", "Validation / Evidence Status",
]

[classes.MEMORY]
path = "MEMORY/NN-<topic>.md"
purpose = "Working memory: intake, source reads, decision and ambiguity registers, context from chat"

[classes.DESIGN]
path = "DESIGN/<name>.md"
purpose = "Intended design, separate from plan status and evidence"

[classes.EVIDENCE]
path = "EVIDENCE/<class>/.../NN-<scope>.md"
purpose = "Proof from real output: validation logs, reports, inventories, comparisons"

[classes.SPIKE]
path = "SPIKE/..."
purpose = "Disposable prototypes and reproducible experiments"

[hooks]
"session.start" = ["brief"]
"dossier.use" = ["brief", "check"]
"checkpoint" = ["codegraph-update", "check"]
```

`~/.agentworkspace/legion-port/dossier.toml` (one dossier):

```toml
title = "Port the Python machine tooling to Rust"

[[entities]]
id = "lenovolegionlinux"
type = "repo"
path = "/data/git/lenovolegionlinux"
base_ref = "main"

[[objectives]]
id = "Y1"
outcome = "legionctl-system plans the same writes as the Python tool for AC, battery, Fn+Q, resume and boot"
status = "in-progress"

[[objectives]]
id = "Y2"
outcome = "No Python installed after the switch; units, udev and sleep hook call legionctl-system"
status = "planned"

[classes.PROBES]
path = "PROBES/NN-<scope>.md"
purpose = "acpi_call read and write probes with the log of each run"
```

## Sandboxed instances

Processes (bwrap), containers and VMs (17) get the same layout: the operating directory mounted read-write as the
working directory, the dossier mounted at `<operating directory>/.agentworkspace`, nothing else writable. A
container or VM for a dossier is recorded in `workspace.db` and reused when the dossier is opened again.

`legionctl workspace pack <dossier> --as container|vm` builds a copy-on-write image (an OCI image with the
operating directory and the dossier as layers, or a qcow2 overlay over a base VM image) together with the session
spec (agent, tools, skills, MCP servers, policy; 14). `legionctl workspace run <dossier>` starts it with the same
mounts, gates and accounting as a local session; changes come back only through the normal merge with approval.
A packed workspace can move to another machine or run several copies in parallel from one base.
