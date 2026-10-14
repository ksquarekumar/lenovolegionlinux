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

## Built (2026-10-13, milestone 1: dossier, binding, gate)

- `legionctl-core::dossier` follows `safecoder dossier` (studied in the source repo): rows `<line>:<hash>|<text>`
  with a BLAKE2b 2-byte hash of the line text (same anchors as safecoder), `replace`, `insert-after` and `delete`
  edits that refuse an outdated anchor, `NN-<slug>.md` numbering per class folder with exclusive create, atomic
  writes, SQLite bindings in `workspace.db` (one open binding per dossier and per session, `steal`), and the 18
  ledger sections in `plan.md`. Search is literal text (no regular expressions).
- Liveness of a binding: each hook call (session start, prompt, stop) and each dossier write refreshes it. A binding
  not refreshed for `dossier.stale_hours` (12) is stale. A session that legionctl started in a scope is stale as
  soon as its scope has ended (after one minute of grace for the client's `systemd-run`), so the next session takes
  its dossier without `--steal`. The end of a session releases its binding (`hook session-end`).
- Dossier nudges (decided 2026-10-13): agents own their dossier, and legionctl tells them to keep it current. At the
  end of each reply the stop hook hashes the workspace (HEAD, status, diff, untracked sizes and times); a reply that
  changed it counts. At `dossier.nudge_turns` (5) counted replies without a dossier write the agent is told on the
  next prompt (info), at twice that the end of one reply is stopped (warn), at three times each reply is stopped
  (block). A write into its own dossier answers it and starts the count again. Each level and each answer is an
  event (`dossier.nudge`, `dossier.ack`); `legionctl dossier nudges [--of <session>]` replays them.
- Format JSONC (decided 2026-10-13). The pattern (artifact classes, plan sections, gate, root, stale time) is the
  `dossier.*` config keys, so it has the layers and locks of 14; each dossier keeps its data (title, entities,
  objectives) in `<dossier>/dossier.jsonc`. There is no separate global pattern file.
- Session id: hooks read `session_id` from Claude Code's hook JSON, except in a legionctl session, where
  `LEGIONCTL_SESSION` replaces it (the dossier is bound to the legionctl session, not to the agent inside it); CLI
  and MCP calls take `LEGIONCTL_SESSION`, else `CLAUDE_CODE_SESSION_ID`, which Claude Code sets for the commands it
  starts.
- `legionctl hook <event>` adapts Claude Code's hooks: `session-start` adds the dossier line to the context, `prompt`
  blocks (`{"decision":"block"}`) when `dossier.gate` is on and the session has no dossier (a prompt that contains
  `legionctl:dossier` passes), `pre-tool` denies `Write`, `Edit`, `MultiEdit` and `NotebookEdit` into a dossier that
  is not the session's, `stop` (before the agent ends a reply) sends a session without a dossier back to set it
  up first, once per reply (`stop_hook_active`), and `session-end` releases the binding. Hook output is for the
  agent first, the user after (decided 2026-10-13); milestone 2 adds the operating directory, branch and copy to
  the `stop` check. Config `dossier.nudge` (default `["session-start", "stop"]`) names the events at which the
  agent is told; at `session-start` it is an instruction in the agent's context before the first prompt.
- Binding follows the place (decided 2026-10-13): each binding records the repository root (else the folder) where
  the session works. At session start (new or resumed) the hook binds the session to the last dossier used there,
  when no live session holds it, and tells the agent (`dossier.auto_bind`, default on). Knowing and using the
  dossier is the agent's job; the nudges and the gate step in only when there is none.
- Milestone 2 (built 2026-10-13): `workspace` replaces `worktree` (15). A workspace is one git worktree per
  dossier and repository at `<workspace.root>/<repo>/<dossier>` on `legionctl/<dossier>`, from a recorded base
  ref, registered in `workspace.db`; `workspace.open` links the dossier as `.agentworkspace` and commits the
  `/.agentworkspace` ignore line on the dossier branch when it is missing. `status` reports ahead/behind, staged,
  unstaged, untracked, conflicts and drift (a branch switched outside legionctl, a missing worktree);
  `checkpoint` commits everything; `merge` rebases onto the base
  and fast-forwards the base in a clean main checkout (decided 2026-10-13); `close` removes the worktree and keeps
  the branch. Writes into the main checkout are not blocked (decided 2026-10-13: nudge only): the session-start
  line names the workspace (with its drift) or says to open one, and the stop hook tells the agent when the main
  checkout has changes and the dossier has no workspace there (`workspace.nudge`).
- Next: plain-folder sources (overlay copies, with sandboxes in 14), then the work graph.

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
  lists recent dossiers for this operating directory and the way forward: `/legionctl:dossier use <name>` (a new
  name makes a new dossier; decided 2026-10-13, no `--create`). It is the only command that passes the gate.
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
| open | A worktree from a recorded base ref on the dossier's branch `legionctl/<dossier>` (created on first open, reused after). If the session started inside the source checkout, the agent is moved to the worktree path. With `--topic <t>`: a separate topic workspace (below). |
| checkpoint | Stage and commit the change set's files with a message from the plan entry; update the code graph. |
| merge | With approval: rebase the workspace's branch onto the base ref, then fast-forward the base in the main checkout; conflicts become git state (14) that the agent resolves in the worktree. |
| close | Remove the worktree; keep the branch until merged; the dossier stays. Uncommitted changes stop it without `--force`. |

Topic workspaces (decided 2026-10-14): parallel agents of one dossier each get their own worktree.
`workspace open --topic <t>` makes the worktree `<workspace.root>/<repo>/<dossier>--<t>` on the branch
`legionctl/<dossier>--<t>` (from the base ref the first time; a reopen uses the branch again), with the dossier linked
as `.agentworkspace`. `status`, `checkpoint`, `merge` and `close` act on the workspace that contains the working
directory, else on the dossier's own workspace in that repository; `--topic <t>` selects a topic workspace from
anywhere in the repository. `workspace list` shows all workspaces, topic workspaces too. An agent that shares its
session's MCP server with other agents gives `topic` (or `repo` as its workspace path) to each workspace tool,
because the server's working directory is the main checkout. The topic workspaces replaced `workspace branch`, which
switched the branch inside the one worktree. `workspace.db` v10 adds the `topic` column (`''` for the dossier's own
workspace); a v9 file goes to v10 at its first open.

Reconciliation runs on every hook call and after every operation. It reads the git state (branch, base ref,
merge-base, ahead/behind, staged, unstaged, untracked, conflicted, resolved-but-not-committed; 14), compares it with
the recorded binding, and repairs or reports drift: a branch switched outside legionctl, a base ref that moved, a
worktree that was removed, a commit made by hand. Git commands that change history or remotes stay gated (14).

A plain-folder source gets an overlay copy; checkpoint snapshots the upper layer, and merge copies changed files
back after a diff and approval.

Built (2026-10-13, W3): `workspace open` (and `session start`) in a folder that is not in a git repository records a
folder workspace (no branch): a layer at `<workspace.root>/<folder>/<dossier>/{upper,work}`, with the dossier link
in the layer. The session sandbox mounts the folder at its own path with `--overlay-src <folder> --overlay upper
work <folder>` (bwrap >= 0.8, no root), so the source does not change. `workspace status` lists the changes from
the upper layer (`A`/`M` files, `D` whiteouts, `R` folders marked `user.overlay.opaque`); `checkpoint` copies them
to `checkpoints/<time>/` with `DELETED` and `MESSAGE`; `merge` applies them to the folder and empties the layer;
`close` drops the layer (it refuses unmerged changes without force); `--topic` is refused. An agent outside a
session works on the folder directly; the layer exists only inside sessions. The e2e test covers an edit, a new
file, a deletion and a replaced folder.

## `~/.agentworkspace`

```
~/.agentworkspace/
  workspace.db                 SQLite (WAL): bindings, locks, artifact index, work graph (graph tables, 13), events
  codegraphs/<repo-id>.db      one ctx graph per unique repo (ctx graph index --db ...), updated at each checkpoint
  <dossier>/
    dossier.jsonc              this dossier only: title, entities, objectives
    plan.md                    PLAN: the ledger
    MEMORY/NN-<topic>.md       working memory, numbered in creation order
    DESIGN/                    PRD.md, HLD.md, LLD.md or scope-appropriate names
    EVIDENCE/<class>/.../NN-<scope>.md
    SPIKE/                     disposable prototypes and reproducible experiments
    <class>/.../NN-<scope>.md  any other class the dossier declares
```

- `<repo-id>` comes from the repo's first commit hash, so clones and worktrees of one repo share one graph. Graphs
  never live inside a repo: ctx's default (`.graf/index.db` in an ancestor) is not used.
- The pattern (classes, plan sections, gate) is the `dossier.*` config (14); a dossier's own `dossier.jsonc` holds
  its data. The canonical classes cannot be removed or renamed.

## Artifact classes and routing

| Class | Path | Holds |
| --- | --- | --- |
| PLAN | `plan.md` | The ledger: state only, short entries that point to the artifact with the detail. Exists from open, may start almost empty. |
| MEMORY | `MEMORY/NN-<topic>.md` | Intake notes, source reads, decision and ambiguity registers, context moved from chat: the resumable core, not a transcript. Usually the first artifact. |
| DESIGN | `DESIGN/...` | Intended design, separate from plan status and from evidence; written after its inputs are settled in MEMORY. |
| EVIDENCE | `EVIDENCE/<class>/.../NN-<scope>.md` | Validation logs, reports, inventories, comparison tables, dumps; written when the check runs, from its real output. Every claim and finding has one. |
| SPIKE | `SPIKE/...` | Minimal, self-contained, disposable prototypes and experiments. Product code enters the product only through an approved change. |
| other | `<class>/.../NN-<scope>.md` | Declared in `dossier.classes` config, for example `HOMEWORK` or `PROBES`. |

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

Built (2026-10-13, W4, first part): the graph is built from the dossier's files on each call (no tables: a dossier
is tens of files, and a graph made on demand is never out of date). Nodes: each `plan.md` table row whose first
cell is an id (`plan#todo-3`, with its `status` column) and each artifact (its front-matter `id`, else its path
without `.md`). Edges: `supports`, `supersedes` and `code` from front matter, and `refers-to` for an artifact path
named in a plan row. `legionctl dossier graph [--node <id>]` shows it; `dossier check` adds dangling links,
acceptance rows that no EVIDENCE supports, and EVIDENCE whose `code` paths have a commit in the dossier's
repository after the file was written. Not yet: code-graph symbols (`touches`), runnable state.

Built (2026-10-13, W4 part 2): status history. Each write through legionctl (`dossier write`, `append`, `edit`,
`add`) compares the statuses of the nodes in the file before and after (the plan rows' `status` column, an
artifact's front-matter `status`) and stores each change in `workspace.db` (`status_changes`: time, session, node,
from, to; null when the node did not exist). `dossier graph --node <id>` returns the node's history, oldest first. A
change made outside legionctl (an editor, a script) is not seen: agents write through the dossier tools. Code-graph
symbols wait until CI has ctx: the code graph is ctx's index, and a test needs it.

`legionctl dossier check` then reports what the practice asks for as concrete findings: missing plan sections, a
claim or a `Done` row without EVIDENCE, an acceptance row with no `verified-by`, a dangling link, a DESIGN that
contradicts an approved decision, and EVIDENCE whose linked symbols changed after it was written (stale evidence).

## One writable dossier per session

| Command | Effect |
| --- | --- |
| `legionctl dossier use <name> [--steal]` | Bind this session to the dossier; a new name makes it from the global pattern. The previous one is released. |
| `legionctl dossier active`, `release`, `locks` | Show the binding; give it up; show which session holds which dossier and stale locks. |
| `legionctl dossier list`, `read <path>`, `search <pattern>` | Read any dossier; lines print as `<line>:<hash>\|<text>`. |
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

## Example

The pattern is config; a repository can add a class in `<repo>/.legionctl/dossier.jsonc`:

```jsonc
{
  "dossier": {
    "classes": { "PROBES": "acpi_call read and write probes with the log of each run" }
  }
}
```

`~/.agentworkspace/legion-port/dossier.jsonc` (one dossier's data):

```jsonc
{
  "title": "Port the Python machine tooling to Rust",
  "entities": [
    { "id": "lenovolegionlinux", "type": "repo", "path": "/data/git/lenovolegionlinux", "base_ref": "main" }
  ],
  "objectives": [
    { "id": "Y1", "outcome": "legionctl-system plans the same writes as the Python tool", "status": "done" },
    { "id": "Y2", "outcome": "No Python installed after the switch", "status": "done" }
  ]
}
```

## Sandboxed instances

Processes (bwrap), containers and VMs (17) get the same layout: the operating directory mounted read-write as the
working directory, the dossier mounted at `<operating directory>/.agentworkspace`, nothing else writable. A
container or VM for a dossier is recorded in `workspace.db` and reused when the dossier is opened again.

### W5: containers and VMs (decided 2026-10-13/14: D5 and round 5)

The agent's changes come back through bind mounts: the operating directory, the repository's `.git` and the dossier
are mounted read-write at their own paths, so the agent writes the worktree directly and the gates (git gate, merge
with approval) stay on the host. Headless runs and interactive sessions both can run inside (`--in container|vm`);
the operator records where each session runs, and pause, unpause, stop and `session list` act on the container or VM
instead of the systemd scope.

`legionctl workspace pack <dossier> [--as container|vm]` makes the image of the dossier's repository:

- Container: the repository's own image (I1 builds its Dockerfile; a repository without one starts from
  `session.container.base`), plus one agent layer: git, Node.js and npm from the image's package manager (apt, apk,
  dnf or pacman), the agents and their ACP adapters from npm (`session.container.npm`), and a static `legionctl`
  (`session.container.client`; the package builds it for musl in `rust:<version>-alpine`) as the MCP server, the
  hook client and the git gate (`/usr/bin/git` is the client; the real git moves to
  `/usr/local/libexec/legionctl/git`). Tag: `legionctl-pack/<project>:<dossier>`.
- VM: a qcow2 disk per repository (`~/.local/share/legionctl/vm/<repo>.qcow2`) over `session.vm.base` (a cloud
  image, downloaded once and checked against its folder's `SHA512SUMS`); one cloud-init boot installs the same agent
  layer and the guest service, then disables cloud-init. A session runs `legionctl vm <spec>` in its scope (so pause,
  unpause and stop act on the scope, as for a host session): a virtiofs daemon per shared folder, then QEMU on a
  throwaway layer over the disk (`snapshot=on`), the agent on the virtio console (the runner's stdio: JSON-RPC for a
  headless run, a terminal for an interactive one). In the guest, `legionctl guest` mounts the shares at their own
  paths (a writable file such as `~/.claude.json` cannot be a virtiofs share: the VM gets its own copy, and the agent's
  changes to it stay in the VM), makes the runtime folder its own with only the session's files shared, and listens on the operator socket's
  path: each connection goes to 10.0.2.100:80, which QEMU's user network forwards to `legionctl relay` on the host,
  into the operator socket. So every client keeps one transport (the unix socket). The agent runs as the user without
  root's groups; the VM powers off when it ends, and after an error (the console log says why). After a headless run,
  in a container or a VM, the operator stops the session where it runs (the real adapters do not end when their input
  closes, and a killed client does not stop its container) and releases its dossier.

`session run|start --in container` starts `docker run` with: the session's user and group; a private tmpfs home with
the agent's account folders (A1: login files written in at start, copied back at end) and `session.writable` paths;
the runtime folder `legionctl/` (operator socket, session files); `LEGIONCTL_SESSION`, `LEGIONCTL_TOKEN`; the normal
bridge network. A plain-folder workspace (an overlay in bwrap) cannot run in a container. GPU only on request per run.

Parts: 1) container pack and headless runs; 2) interactive container sessions and resume; 3) VMs.

Built (2026-10-14, part 3): `workspace pack --as vm` and `session run|start --in vm` as above. QEMU runs with
`smm=off`: on this machine's kernel, KVM resets a SeaBIOS guest with SMM at once (a reboot loop after GRUB); SMM is
only for Secure Boot firmware, which these disks do not use. Test (e2e, KVM; CI's own step): pack from the Debian
cloud image, a headless run and an interactive session through the real client, commits in the host workspaces,
the push refused by the disk's gate through the guest relay, the dossier shared, the home private.

Built (2026-10-14, parts 1 and 2): `workspace pack [--service <name>]` makes one image per repository (the agent
layer does not depend on the dossier, since D5 mounts the work), `localhost/<repo>/legionctl-agent:latest`, as a job
(resumed after a restart); `session run|start --in container`; the sessions table records `inside`, and pause,
unpause, stop, `session list`, binding liveness and the idle freeze use the container. The client adds `-t` when it
runs in a terminal. The gateway (2026-10-14): when `gateway.port` is set, the operator also serves it on
`$XDG_RUNTIME_DIR/legionctl/gateway.sock`; a container's agent runs under `legionctl forward <port> <socket> --` (the
image's static client), and a VM's `legionctl guest` listens on 127.0.0.1:<port> and relays to 10.0.2.100:81, which
QEMU forwards to `legionctl relay <gateway socket>`. So `ANTHROPIC_BASE_URL`/`OPENAI_BASE_URL=http://127.0.0.1:<port>`
works in a sandbox as on the host, and the TCP port stays on the host's loopback. GPU per run (2026-10-14, the
user: containers and VMs): `session run|start --gpu`. On the host, bwrap gets the NVIDIA nodes and `/dev/dri` (a
session without `--gpu` sees none); in a container, `--device nvidia.com/gpu=all` (the NVIDIA Container Toolkit's
CDI spec; the host's driver); in a VM, the runner binds the NVIDIA GPU's whole IOMMU group to vfio-pci for the run
(`vfio bind` with its own pid and the VM's memory plus 1 GiB to lock; a polkit dialog; refused while the desktop or
any process uses the GPU) and gives it back after, also after an error. A VM needs a disk made with `workspace pack
--as vm --gpu` (the generic Debian kernel, `nvidia-open-kernel-dkms`, the GSP firmware, `nvidia-smi`, `libcuda1`
from non-free; a mark beside the disk), else `--gpu` is refused with that command. A resumed session runs without the
GPU. A start that fails after its dossier was bound releases it. Tests: the host session sees the nodes only with
`--gpu` (mutation-checked; without NVIDIA nodes the start is refused with the reason); the VM test refuses `--gpu` on
a disk without the driver and the dossier stays free; an ignored test packs with `--gpu` and finds the built module
(slow; run by hand). Not tested live: a GPU container (rules: no live GPU containers by the agent) and a GPU VM (the
user's first run).
Test (e2e, docker): the repository's Dockerfile image gets the layer; a headless run and an interactive session
through the real client commit into their workspaces on the host, the gate in the image refuses the push (an agent
layer without the gate fails the test), the dossier is writable, the home is private, and a run without a pack says
how to make one.
