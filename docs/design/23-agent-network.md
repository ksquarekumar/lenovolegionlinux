# 23 — Agent network, two code graphs and the thread UI

Status: proposed (2026-10-15). Plan rows todo-90 (two code graphs), todo-91 (agent network), todo-92 (thread UI).
The user reviews this design before any part is built. The user's request (verbatim) and the decisions are in the
dossier: `MEMORY/11-agent-network-and-code-graphs.md`.

## In short

| Part | What | Main choice |
| --- | --- | --- |
| A. Actual-code graph (todo-90) | One ctx graph snapshot per git tree hash | `ctx graph index` of the exported tree into `.graf/refs/<tree-hash>.db`; ctx does the parse, legionctl does refs, diff and annotations |
| A. Change graph (todo-90) | Entity-level and event-level changes between two refs | Diff by stable symbol keys (ctx binding aliases), not by ctx node ids |
| A. Planned-code graph (todo-90) | Features, objectives, stories, work items, planned units | JSONC files in `planning/` in the repository, versioned with the code |
| A. Tests (todo-90) | Tests linked to stories, test runs, test review | Mutation kills per test are the main signal; the gate is `workspace checkpoint` |
| B. Agent network (todo-91) | `#main0` and observers in one durable thread | Recommended: observers are operator ACP sessions with a durable outbox; messages reach the main agent through its hooks |
| C. Thread UI (todo-92) | Triple pane, chat bar, `/` commands, views | One layout in the TUI (ratatui) and the app (GPUI); views are declarative JSONC from a closed widget set |

Each part has: what, why, data model, API, hooks, storage, failure and restart behaviour, tests and phases. The
questions for the user are at the end.

## Coverage of the request

Each phrase of the user's request maps to a section. "Open" means a question at the end.

| Phrase of the request | Section |
| --- | --- |
| change detection tooling for agents | A3 Change graph |
| actual-code codegraph v/s planned-code codegraph | A1, A4, A5 Plan against actual |
| entity-level and event-level annotations | A3 (entities, events), A6 Annotations |
| at/across ref levels since/from/across [ref1, ref2, ...] | A3 (`--from`, `--to`), A6 (`--refs`) |
| so long as those refs have .graf in them | A2 (a ref "has .graf" when its snapshot exists) |
| tokei for LOC actual across features/workitems-planned | A5 Plan against actual |
| a graph for every current/chosen ref, change-diff hash, content hash, file path hash | A2 Addresses |
| maps changes against its base/previous ref | A3 (base = first parent, or the workspace base) |
| resolving-git-conflicts/rebasing skills should remember this | A8 Rebase and conflicts |
| agents estimate work across files, paths, modules, namespaces when they plan | A4 Planned-code graph |
| global, repo-root addressable namespacing for all units of code/config for run/build/deploy | A4 Unit keys |
| change detection | A3 |
| test-coverage detection and management; tracks and runs tests | A7 Tests |
| reviews tests for noise, fluff, obviousness, low value; prevents submission by hook | A7 Test review and gate |
| triggers agents to observe, react, send and receive messages to and from the main agent | B Agent network |
| agents in a thread share access to the main agent's dossiers and work in their own | B4 Dossiers and workspaces |
| run infrequently (when main stops or declares), or poll, watch, react (configurable) | B5 Pace |
| #dev-agent-teams archetype: immutable multi-agent thread, compensation, durability, outbox, observable, branchable, concurrent | B2 Data model, B6 Failure, B7 Branch |
| some can watch as they declare | B5 (`declare` events) |
| observer leaves messages, soft-blocks, hard-blocks | B3 Message kinds, B4 Delivery |
| runs tools main#0 forgot and reminds main#0 with the count since observer#0 started watching | B3 Watch rules and counts |
| the third agent: persistable, recomposable, reusable UI components for the query, task, progress, quality metrics, tests | C4 UI-component agent |
| in TUI/app, when legionctl is used directly on an account (Claude or Codex) | C1, C5 Accounts |
| threads do not switch across accounts; they can be imported | C5 Threads per account |
| dynamic plugged data views, triple pane, pane widths 5-10%, 20-40%, 50-75% | C2 Layout |
| panes 1 and 2 collapse to folded vertical columns | C2 |
| chat bar centred at the bottom, grows wider, grows taller with content | C2 Chat bar |
| same view for agents and humans | C3, C4 (`views_render`) |
| "/" commands for tool-like use; expose the agent's own commands | C3 Commands |
| if the harness or ACP does not allow it, run a command to get the list | C3 (scan of command folders) |
| does ACP support user-invoked commands to the harness? | D1 |
| is `legionctl:tool-<toolset-toolname>` addressable as `<toolset>/<toolname>` in searches? | D2 |
| or a mode where the plugin spawns subagents for observers, addressable and discoverable | B1 Option B and C, D2 |
| hooks log, save, inform and trigger each agent's state; real-time change updates from main | B4, B5 |
| each observer decides by its role, query, task, progress, tests, reviews | B1 Roles |
| main agent is responsible for the whole task with verifiable reviews, tests, fixes | B1, A5 (completeness) |
| across well-catalogued dossiers that survive restarts, crashes, compactions | B6 Failure and restart |
| features, objectives, stories with lifecycles, data flows, control flows, requirements, issues | A4 Format |
| each tested and linked via the tests/e2e suite | A4 (`tests`), A7 |
| planner knows quality, correctness, completeness measures, what they last measured and when | A5 (`plan status`), A7 (test runs) |
| 2 graphs with triggers; agent-easy API; enforced by hooks | A2 Refresh, A9 API, A7 gate |
| fire subagents to watch main, fire tools, send messages and hints that main misses | B |
| all use the dossier as memory; store the embedded graphs in the dossier? | A10 (no: decisions 1 and 2; the dossier keeps links) |
| bound to one dossier does not mean it cannot read others; all agents read all dossiers | B4 (reads are open today) |

## What exists today (research, 2026-10-15)

### ctx 2.2.8 (`~/.local/bin/ctx`)

Checked with `ctx --help`, `ctx graph <cmd> --help`, the MCP tool schemas and two test indexes of this repository
(HEAD `02141c7`, tree `08c72c3c`, and HEAD~3) into scratch databases.

| Question | Fact | Effect on the design |
| --- | --- | --- |
| What does ctx record? | SQLite tables `files` (path, content hash, module), `nodes` (id, label, kind, file, line, end_line, qualified_name, metadata JSON with `binding_aliases`, `start_byte`, `end_byte`, `language`, `public`), `edges` (source, target, relation, confidence, file, line), `refs` (unresolved and resolved references), FTS5 `node_search`, `metadata` (root, generation, coverage). | Reuse as is. No new parser. |
| Entities in this repo | 4693 nodes: function 2911, method 691, struct 311, impl 177, constant 191, module 131, enum 56, config_key 53, package 9, mcp_server 1, ... | Symbols, modules, crates and config keys come from ctx. |
| Edges in this repo | 8287 edges: contains, calls, imports, parameter_type, type_argument, impl_type, implements, crate_depends_on, depends_on, return_type, field_type, ... | `graph impact` and `callers` work for Rust. |
| Can it index a given tree? | Yes. `ctx graph index <dir> --db <file> --code-only`: the exported tree (`git archive <tree>`) took 1.0 s (121 files). | One snapshot per tree hash is cheap. |
| Incremental from a parent snapshot? | `index root differs from the stored root` when the folder differs. With one fixed folder it worked, but took 2.4 s (more than a full index). | Always a full index. No staging folder. |
| Size | 37 MB per snapshot (4.9 MB with zstd). | Prune (A2). |
| Are node ids stable across refs? | No. Ids hold byte offsets (`rust:crates/legionctl-core/src/graph.rs:Node@708`). HEAD~3..HEAD by id: +1281 / −1174 nodes. By binding alias (`rust:crates/legionctl-core:graph::Node`, offsets removed from closures): +85 / −2. | Diff and annotations use the stable key (A3). |
| Can queries take the stable key? | No. `graph impact`, `show` take an exact id or a unique label; the alias gives "no symbol matches". | legionctl maps a key to the node id of each snapshot. |
| Can it diff two graphs? | No diff command. `graph merge` composes projects into one new database; `check-update` compares files with the source folder. | legionctl does the diff (two read-only SQLite reads). |
| Can it attach data? | Only `save-result` / `--memory-dir` (saved answers that cite node ids). | legionctl keeps its own annotations by stable key. |
| Per-node content hash? | No. Byte ranges are there. | legionctl hashes the byte range of the git blob. |
| MCP | `ctx mcp` serves `graph_query`, `graph_impact`, `graph_callers`, `graph_callees`, `graph_path`, `graph_show`, `graph_stats`, `blame`, `search` on one database chosen at start. `ctx graph serve --project NAME=DB` adds named databases at start. | Agents keep ctx's tools for the working copy. legionctl tools give ref-aware reads by `--db`. |
| Git hooks | `ctx graph hook install` refreshes one `.graf/index.db` after commit, checkout, merge. | Not used: it keeps one mutable index per checkout, against decision 1. legionctl installs its own hook. |
| Provenance | `ctx blame commit <sha>` gives the agent session that made a commit. | Source for event-level annotations. |
| Gaps | C with eBPF macros and the kernel module (5 files): "Syntax error; no facts indexed". | These files are file-level only (tokei and the git blob hash). |
| Main checkout today | `.graf/index.db` is from 2026-10-12, generation 1, 11 files (docs and the old Python tool), no Rust. `.graf/` is in the user's global git ignore. | It is out of date; snapshots replace it. |

### tokei 15.0.0

`tokei -o json <paths>` gives per language and per file `code`, `comments`, `blanks` (and embedded languages in
`blobs`). The whole tree takes 30 ms. tokei counts files only; it does not count a line range. A symbol's size comes
from ctx's `line..end_line` (a span, with blanks and comments).

### legionctl today

| Part | Fact | Source |
| --- | --- | --- |
| Work graph | Built on demand from the dossier's files: plan rows and artifacts as nodes; `supports`, `supersedes`, `code`, `refers-to` edges; `status_changes` history in `workspace.db`; `touches` to code-graph symbols "waits until CI has ctx". | 18-workspaces.md, `crates/legionctl-core/src/graph.rs` |
| Hooks | `session-start`, `prompt`, `pre-tool` (Write/Edit), `stop` (nudges: info, warn, block), `session-end`; hook actions per event with `hooks add`/`bind`. | `integrations/agent-plugin/hooks/hooks.json`, 18-workspaces.md |
| Sessions and ACP | `session run` drives `claude-code-acp` / `codex-acp` with the operator's ACP client (crate `agent-client-protocol` 1.3.0, schema 1.4.0); answer chunks and tool calls are `session.output` events; `session/load` continues a cut-off run; other session updates are dropped (`_ => {}` in `acp.rs` `update`). | 14-sessions.md, `crates/legionctl-operator/src/acp.rs` |
| Events | `GET /v1/events` server-sent events (offset or `now`, catch-up then live), `events`, `trace`. | 16-observability.md |
| Jobs | `jobs` table (kind, input, state, attempts); the operator resumes running jobs after a restart; `Db::job_step` resets the attempt count. | `crates/legionctl-core/src/db.rs` |
| Outbox and sagas | Designed (event log, outbox, sagas with compensation), not built as tables. | 13-daemon.md |
| Topic workspaces | One worktree per agent of one dossier, merge one by one. | workspace-practice skill |
| Accounts | Account profiles with their own `CLAUDE_CONFIG_DIR`; `session resume --account` copies the Claude Code transcript into the other profile. | e2e `a_resumed_session_continues_on_another_account_with_its_transcript` |
| TUI | `legionctl tui` (ratatui), a live dashboard. | `crates/legionctl/src/tui.rs` |
| App | `legionctl-app` on gpui-kit 0.7.0: Now, Apps & GPU, Health, Bench pages; polls every 2 s. | 09-ux.md |
| Registry | Each operation declared once; it makes HTTP, MCP tool (`a_b_c`), CLI (`a b c`), polkit and event kind. | 15-layout.md |
| Test evidence | Mutation checks are made by hand and listed in each commit message ("Mutation-checked (each made its test fail): ..."). `cargo-mutants` and `cargo-llvm-cov` are not installed. 11 `#[test]` blocks are in `src/`; 5 integration test files with 72 functions in `tests/e2e.rs`. | git log, `crates/*/tests` |

## Part A: two code graphs (todo-90)

### A1. What and why

- The actual-code graph tells what the code is at a ref. The planned-code graph tells what the code must become.
  The change graph tells what changed between two refs, and who changed it.
- Agents lose this state at a restart or a compaction. With the graphs, an agent asks "what is left of work item
  W3" or "which symbols did this rebase touch" and gets facts, not a reread of the code.
- ctx already parses and stores code. legionctl adds refs, diffs, annotations, plans, LOC and tests. legionctl does
  not parse source code.

### A2. Snapshots per tree hash (decision 1)

| Item | Rule |
| --- | --- |
| Address | The git tree hash of a commit (`git rev-parse <ref>^{tree}`). Two commits with the same tree share one snapshot. |
| Path | `<common dir>/../.graf/refs/<tree-hash>.db`, where `<common dir>` is `git rev-parse --path-format=absolute --git-common-dir`. So all worktrees of a repository use the main checkout's `.graf/refs/`. |
| Git | `.graf/` is ignored (already in the user's global ignore; `workspace open` adds `/.graf/` to the repository's `.gitignore` when it is not ignored). |
| Build | `git archive <tree> \| tar -x` into a new temporary folder, `ctx graph index <folder> --db <tree>.db.tmp --code-only --json`, then `rename` to `<tree>.db`, then remove the folder. `git archive` gives only tracked files. |
| Concurrency | The file name is the content address. Two builders of one tree both write a `.tmp` with their own suffix; the first `rename` wins, the second removes its file. |
| Immutable | A snapshot is never updated. `ctx graph update` is never run on it. |
| "Has .graf" | A ref has `.graf` when `refs/<tree-hash>.db` exists. Each read that names refs reports each ref with no snapshot. `--build` makes the missing ones. |
| Working copy | Uncommitted changes have no tree hash. `code graph changes --to work` makes a snapshot of `git stash create` (a commit of the working copy, not stored in the stash list) under its tree hash. |

Addresses that the user named:

| Address | Meaning | Where |
| --- | --- | --- |
| ref (branch, tag, commit) | resolved to a commit, then its tree | `git rev-parse` |
| tree hash | content address of a whole snapshot | `.graf/refs/<tree>.db` |
| change-diff hash | BLAKE2b-128 of `<base tree>..<tree>`: the id of one change set | `.graf/changes/<diff-hash>.json` |
| file path hash | the git blob id of the file at that tree | `git ls-tree -r <tree>`, stored with each file entity |
| symbol content hash | BLAKE2b-128 of the symbol's byte range in its blob | computed with the change set |

Refresh triggers (decided 2026-10-15):

| Trigger | Action |
| --- | --- |
| `workspace checkpoint` | After the commit, a job `graph.snapshot` for the new tree. |
| `workspace merge` | A job for the new base tree. |
| `hook session-start` | A job for the HEAD tree of the session's workspace, when it has no snapshot. |
| Git hook | `legionctl code graph hook install` adds `post-commit`, `post-checkout`, `post-merge`, `post-rewrite` lines that call `legionctl code graph refresh --background` (it returns at once). It adds lines with a marker and does not replace another hook. |

Prune (`code graph prune`, also at each refresh): keep the trees of each branch head, each workspace base and its
merge-base with the head, each tree that an open thread names, and the newest `graph.keep` (20) others. Remove the
rest. 37 MB each: about 1.5 GB at the limit for this repository. The question about retention is at the end.

### A3. Change graph

`code graph changes --from <ref> --to <ref|work>`. Without `--from`, the base is the first parent of `--to`, or for a
workspace branch its recorded base (merge-base). Output: one change set, cached as `.graf/changes/<diff-hash>.json`
(content-addressed, so a cache entry is never out of date).

Stable key of an entity (the namespace of A4):

1. Take the first `binding_aliases` entry of the node that is not a `package` alias (for Rust:
   `rust:<crate dir>:<module path>::<name>`, for a file: `file:<path>`).
2. If there is none, take the node id and remove each `@<byte offset>` part.
3. Add the node `kind`. Two nodes with the same key and kind get `#2`, `#3` in file order.

Entity-level rows:

| Field | Meaning |
| --- | --- |
| `key`, `kind`, `file` | The stable key, the ctx kind, the file |
| `change` | `added`, `removed`, `modified` (content hash differs), `moved` (same key and content hash, other file); a rename is `removed` plus `added` |
| `lines` | `+a −r` from `git diff --numstat` for files; the span change for symbols |
| `impact` | count of incoming dependencies at `--to` (`ctx graph impact --db`), on request |

A line inserted above a function does not make it `modified`: the key and the content hash stay the same.

Event-level rows: for each commit in `from..to`: sha, time, author, the checkpoint event (`workspace.checkpoint`
with session and dossier), the agent session (`ctx blame commit <sha>`), the job and the plan rows named in the
commit message (`todo-NN`). Each entity row links to the commits that touched its file lines.

### A4. Planned-code graph (decision 2: in the repository)

Folder `planning/` at the repository root, versioned with the code. A branch carries its plan, a merge merges it,
and each tree snapshot has the plan of that tree. The format is JSONC (decided for dossiers, 2026-10-13). One file
for each feature, because parallel agents then change different files.

```text
planning/
  README.md              how to plan (generated from this section)
  units.jsonc            run/build/deploy units that ctx does not index (services, packages, images)
  features/
    agent-network.jsonc  one feature: objectives, stories, work items
    code-graphs.jsonc
```

```jsonc
// planning/features/code-graphs.jsonc
{
  "id": "code-graphs",
  "title": "Two code graphs",
  "status": "planned",            // planned | in-progress | done | abandoned
  "plan_rows": ["legionctl#todo-90"],   // dossier links: <dossier>#<row>
  "objectives": [{ "id": "O1", "text": "An agent sees what changed between two refs, per symbol." }],
  "stories": [
    {
      "id": "S1",
      "kind": "story",            // story | lifecycle | data-flow | control-flow | requirement | issue
      "text": "A symbol that moves down in its file is not reported as changed.",
      "objectives": ["O1"],
      "tests": ["test:crates/legionctl/tests/e2e.rs::changes_between_two_refs_are_entity_level"]
    }
  ],
  "work_items": [
    {
      "id": "W1",
      "stories": ["S1"],
      "status": "planned",
      "depends_on": [],
      "units": [
        { "key": "file:crates/legionctl-core/src/codegraph.rs", "change": "add", "loc": 450 },
        { "key": "rust:crates/legionctl-core:codegraph::changes", "change": "add" },
        { "key": "op:graph.changes", "change": "add" },
        { "key": "config:graph.keep", "change": "add" }
      ]
    }
  ]
}
```

Unit keys: one repo-root namespace for code, config, run, build and deploy.

| Prefix | Unit | Resolved against |
| --- | --- | --- |
| `file:<path>` | any tracked file | the snapshot's `files`, the tree |
| `rust:<crate dir>:<path>::<name>` (and other ctx language aliases) | symbol, module | the snapshot's `binding_aliases` |
| `rust:package:<crate dir>:<crate>` | crate | ctx `package` nodes |
| `config:<dotted key>` | a legionctl config key | the config schema (`legionctl validate`) |
| `op:<operation>` | a registry operation | `docs/api/schema.json` |
| `test:<file>::<fn>` | a test | ctx function node in a `tests/` file |
| `unit:<name>` | systemd unit, package, image, hook, polkit action | `planning/units.jsonc` (each unit names its files) |

`legionctl plan check` refuses a file that does not parse, a duplicate id, a link to an unknown id and a unit key
with an unknown prefix. A unit that does not exist yet is correct for `change: add`.

### A5. Plan against actual

`plan status [--feature <id>] [--ref <ref>]` joins the planned graph of a tree with its actual snapshot:

| Measure | How |
| --- | --- |
| Unit present | The key resolves in the snapshot (`add`, `edit`) or is gone (`remove`). |
| Unit changed | The key is in the change set from the feature's start ref (the first commit that added the feature file) to `--ref`. |
| LOC actual | `file:` units: tokei `code` of the file at the tree. Symbol units: ctx span `end_line − line + 1` (marked as span). Rolled up to work items and features. |
| LOC planned | The sum of `loc` estimates. Shown with the actual and the ratio. |
| Tests | Each story's tests: present in the snapshot, last result at this tree or an older one (with its age), mutation kills (A7). |
| Completeness | A work item is `complete` when each unit is present and changed, and each test of its stories passed at this tree. The planner sets `done`; `plan check` reports a `done` item that is not complete. |
| Last measured | Each measure shows the tree, the time and the job that made it. |

`dossier check` gets two findings: a plan row with no feature that names it, and a `done` row whose feature is not
complete at the dossier's workspace head.

### A6. Annotations

Table `annotations` in `workspace.db`:

| Column | Meaning |
| --- | --- |
| `id`, `repo` | row id; the repository (common dir) |
| `key` | stable key of the entity, or `commit:<sha>`, `event:<id>`, `job:<id>` for an event-level annotation |
| `kind` | `note`, `review`, `test`, `loc`, `evidence`, `block`, or a free kind |
| `value` | JSON |
| `tree`, `content_hash` | the tree and the symbol content hash when it was written |
| `session`, `agent`, `created` | who and when |

`code graph annotate <key> --kind <k> --value <json> [--ref <ref>]` writes one.
`code graph annotations [--key] [--refs r1,r2,...]` reads them. For each ref with a snapshot, an annotation is
`valid` when the key exists and its content hash is the same, `stale` when the key exists with other content, and
`absent` when the key is not there. Refs without a snapshot show `no graph`. So an annotation follows its symbol
across refs and rebases.

### A7. Tests: links, runs, review and the gate

The user's rule: tests are e2e and of high diagnostic value only; obvious tests are banned. Mutation checks are the
evidence today.

Runs: `code tests run [--filter]` runs `tests.command` as a job. The default command is
`cargo test --release --workspace --no-fail-fast -j8`. The job reads the libtest lines
(`test <name> ... ok|FAILED|ignored`) and stores each result in `test_runs` (tree, test key, result, ms, job, time).
Tests that need root or a VM keep `#[ignore]` and show as `ignored`, not as `failed`.

Mutation records: table `mutants` (tree, file, line, description, the tests run, the tests that failed = killed,
source `tool` or `agent`). Two sources:

| Source | How | Cost |
| --- | --- | --- |
| Tool | `cargo-mutants` with `--file` limited to the files of the change set (A3), and `--test-tool` set to the suite | One suite run per mutant: minutes. Not installed today. |
| Agent | `tests mutant record --file --line --description --killed-by <test>...` after a hand-made mutation, as today | No tool; the agent must do it. |

Signals for a low-value test (each computed per new or changed test of the change set):

| Signal | Rule | Level |
| --- | --- | --- |
| No kill | The test kills no mutant of the change set (it passes with each mutation). | hard |
| No assertion | No `assert!`, `assert_eq!`, `assert_ne!`, `unwrap_err`, `panic!` in the test body (ctx span). | hard |
| Internals only | The test's callees (ctx `graph_callees`, depth 3) reach no boundary: the CLI binary run helper, HTTP `/v1/ops`, D-Bus, MCP, `legionctl hook`. | soft |
| Duplicate | Its killed mutants are a subset of another test's killed mutants. | soft |
| Unit test in `src/` | A new `#[test]` in `src/` (the rule says e2e only). | soft |
| Constant only | Each assertion compares with a literal that the code under test returns as a literal (for example help text). | soft, reviewed by the test-reviewer observer |

Hook points:

| Point | What it does |
| --- | --- |
| `pre-tool` on `Write`/`Edit` of a test file | Static signals only (no assertion, unit test in `src/`). A note to the agent. Does not deny. |
| Observer `test-reviewer` at `stop` | Runs the cheap signals, reads the test, and sends `soft-block` or `hint` messages (B3). |
| `workspace checkpoint` | Gate: each new or changed test needs a mutant record with a kill at this change set. A hard signal refuses the checkpoint with the test names and the reasons. `--accept-test <test> --reason <text>` passes one; the reason is an annotation and an event. |
| `pre-commit` (`.pre-commit-config.yaml`) | `legionctl tests review --staged` with the same rules, for commits made outside legionctl. |

### A8. Rebase and conflicts

The workspace-practice skill gets a section "Rebase and conflicts":

1. Before you resolve a conflict, run `code graph changes --from <merge-base> --to <ours>` and `--to <theirs>`.
2. Resolve per entity: an entity that both sides changed is the real conflict. Other hunks are position only.
3. After `git rebase --continue`, the checkpoint makes the new snapshot. Run
   `code graph annotations --refs <old head>,<new head>`: a `stale` annotation tells you which evidence to check again.

The memory row `ref-change-graph-for-rebase` points to this section.

### A9. API

New group `code` (see question 5). Each operation is in the registry, so it is a CLI command, an MCP tool and an HTTP
call.

| Operation | CLI | MCP tool | Class |
| --- | --- | --- | --- |
| `code.graph.refresh` | `code graph refresh [--ref] [--background]` | `code_graph_refresh` | session |
| `code.graph.refs` | `code graph refs` (snapshots, size, refs that point to each) | `code_graph_refs` | read |
| `code.graph.query` | `code graph query <text> [--ref]` (ctx `graph search --db`) | `code_graph_query` | read |
| `code.graph.impact` | `code graph impact <key> [--ref] [--depth]` (key mapped to the node id) | `code_graph_impact` | read |
| `code.graph.changes` | `code graph changes [--from] [--to] [--impact]` | `code_graph_changes` | read |
| `code.graph.annotate` | `code graph annotate <key> --kind --value` | `code_graph_annotate` | session |
| `code.graph.annotations` | `code graph annotations [--key] [--refs]` | `code_graph_annotations` | read |
| `code.graph.prune` | `code graph prune [--dry-run]` | `code_graph_prune` | session |
| `code.graph.hook` | `code graph hook install\|remove` | `code_graph_hook` | workspace |
| `code.plan.check` | `code plan check [--ref]` | `code_plan_check` | read |
| `code.plan.status` | `code plan status [--feature] [--ref]` | `code_plan_status` | read |
| `code.tests.run` | `code tests run [--filter]` (a job) | `code_tests_run` | session |
| `code.tests.review` | `code tests review [--staged] [--ref]` | `code_tests_review` | read |
| `code.tests.mutant` | `code tests mutant record\|run` | `code_tests_mutant` | session |

ctx's own MCP tools stay for the working copy. legionctl does not wrap them one by one.

### A10. Storage, failure and restart

| Data | Place | Why |
| --- | --- | --- |
| Snapshots, change sets | `.graf/refs/`, `.graf/changes/` (ignored) | Content-addressed, made again from git at any time |
| Planned graph | `planning/` (versioned) | Decision 2 |
| Annotations, test runs, mutants | `workspace.db` | Durable history; not content of the repository |
| Dossier | links only (`code:` front matter with unit keys, `plan_rows`) | Decision 1 and 2 put the graphs outside the dossier. A graph in the dossier would be a second copy. |

- A refresh is a job. A cut-off job starts again from the archive step; a `.tmp` file older than one hour is removed.
- A snapshot that ctx refuses (a ctx error) is not written. The read says `no graph` with the job's error.
- A ctx upgrade with a new schema: `code graph refs` shows the ctx version of each snapshot (`metadata`), and a read
  of an old one makes it again.

### A11. Tests (e2e, high diagnostic value)

| Test | It catches |
| --- | --- |
| `a_snapshot_is_made_once_per_tree_and_shared_by_worktrees` | Two worktrees on one tree and two concurrent refreshes give one file; a checkout of an old ref uses its snapshot; prune keeps the bases and heads. |
| `changes_between_two_refs_are_entity_level` | A function that only moves down is not `modified`; a body edit is; a symbol moved to another file is `moved`; the commit, the checkpoint event and the session are in the event rows. |
| `an_annotation_follows_its_symbol_across_refs_and_turns_stale` | `valid` after an unrelated edit and a rebase, `stale` after a body edit, `no graph` for a ref with no snapshot. |
| `a_work_item_is_complete_only_when_its_units_exist_and_its_tests_pass` | LOC actual from tokei against the estimate; a failed story test keeps the item incomplete; `dossier check` reports the `done` row. |
| `a_test_that_kills_no_mutant_is_refused_at_checkpoint` | The gate refuses, `--accept-test` passes with a recorded reason, a duplicate test is a note only. |

Mutation checks for these: the diff by node id, a missing content hash, a gate that ignores changed tests.

## Part B: agent network (todo-91)

### B1. What, why and how observers run

A thread is one task with a pool of agents: `#main0` owns the task and its completion. Observers (`#observer0`,
`#observer1`, ...) watch the main agent's events and act by role. The main agent forgets steps, writes weak tests
and loses state at a compaction. An observer with its own context remembers and checks.

Roles (one file each, `integrations/agent-plugin/agents/<role>.md`, used as the observer's instructions):

| Role | It watches | It does |
| --- | --- | --- |
| `test-reviewer` | test files in the change set, `code tests` results | runs `code tests review`, mutation runs; soft-blocks or hard-blocks weak tests |
| `process` | tool calls, dossier writes, checkpoints | reminds forgotten steps (lint, dossier record, checkpoint) with counts |
| `verifier` | `declare` events ("done", "ready") | runs the suite and `code plan status`; reports what is not complete |
| `ui-composer` | the thread's query and progress | makes and updates views (C4) |

How observers run (open question 1):

| Option | How | For | Against |
| --- | --- | --- | --- |
| A. Operator ACP sessions (recommended) | Each observer is a legionctl session run through its ACP adapter, with its own dossier, topic workspace and account; the operator wakes it with a prompt that holds the new events. | Durable (jobs, `session/load`), works for Claude and Codex, own sandbox and budget, runs when the main session ends, visible in `session list` and events. | Separate model cost; the real adapters are not checked live yet (14-sessions). |
| B. Claude Code subagents | Plugin `agents/<role>.md` gives subagent types `legionctl:test-reviewer` and others; the main agent starts them. | No new runtime; the main agent can call one at once. | Starts only from the main agent's turn; ends with the session; no durable state; Claude only; the main agent must remember to start it (the failure that observers must fix). |
| C. Both | The role files serve as subagent types and as the ACP observers' instructions. | One source of roles; a quick check in the main turn. | Two run paths to test. |

Recommendation: A for observers. The same role files also exist as plugin agents (option C at no extra cost),
because Claude Code reads `agents/` from the plugin; but only A is a member of a thread.

Deterministic watch rules (B3) run in the operator without a model. A model observer is used for judgment only.

### B2. Data model

All in `workspace.db`. The thread log is append-only (events table, `thread` column). The other tables are
projections, written in the same transaction as their event.

| Table | Columns |
| --- | --- |
| `threads` | id, title, account, root prompt, dossier, repo, parent thread, parent event (for a branch), created, state (`open`, `closed`) |
| `members` | thread, name (`main0`, `observer0`), role, session id, pace, cursor (last event id seen), state |
| `messages` (the outbox) | id, thread, from, to, kind, body, refs (keys, events), state (`pending`, `delivered`, `acked`, `retracted`, `expired`), created, delivered, acked |
| `watch_rules` | id, thread, member, trigger (event pattern), expect (event pattern), window (count or seconds), message kind |
| `watch_counts` | rule, count, first seen, last seen (since the member started to watch) |
| `effects` | id, member, kind (`checkpoint`, `tests.run`, `message`), ref, compensation (`revert <sha>`, `retract <msg>`), state |

### B3. Messages, watch rules and counts

| Kind | Effect on `#main0` |
| --- | --- |
| `note` | Shown in the next prompt's context. |
| `hint` | Shown in the next prompt's context, with the facts that the observer verified. |
| `reminder` | From a watch rule: "observer0: no `make lint` after 4 edits of `crates/` — 3rd time since 10:42". |
| `soft-block` | The `stop` hook stops the end of one reply with the reason (like a nudge `warn`). |
| `hard-block` | The `pre-tool` hook denies the matched tool (for example `workspace_checkpoint`) until the message is acked or retracted; the `stop` hook stops each reply. |
| `result` | A test run, a review or a patch (a commit on the observer's topic branch). |

A watch rule example: trigger `agent.tool` with `Edit` on `crates/**`, expect `agent.tool` with `Bash(make lint*)`
within the same reply, kind `reminder`. At each miss the count goes up, and the message carries the count since the
member joined. The `process` role has default rules: lint after a code edit, dossier write after a checkpoint, the
suite before `declare done`.

### B4. Delivery, dossiers and workspaces

Into the main agent (both Claude Code in a terminal and a session through ACP, because Claude Code runs the plugin
hooks in both; to check live with `claude-code-acp`):

| Hook | Reads | Gives |
| --- | --- | --- |
| `prompt` | pending `note`, `hint`, `reminder`, `result` | `additionalContext`; marks them `delivered` |
| `pre-tool` (matcher extended to all tools) | open `hard-block` messages that name the tool | deny with the reason |
| `stop` | `soft-block`, `hard-block` | block with the reason |
| new `post-tool` | the tool name, the path, the outcome | records an `agent.tool` event (the observers' input) |

The main agent answers with `thread ack <msg> [--reply <text>]` and announces a state with
`thread declare <word> [--note]` (`done`, `ready`, `blocked`, `tests-written`).

Dossiers: each observer has its own dossier `<main dossier>--<member>` (for example `legionctl--observer0`) and
writes only there. It reads the main dossier and all others (reads are open today). Workspaces: each observer that
changes files uses its own topic workspace (`workspace open --topic <member>`). It never writes in the main
agent's workspace. A patch is a `result` message with its commit; the main agent takes it (`git cherry-pick`).

### B5. Pace

`members.pace`, set at `thread join` and changed by `thread pace`:

| Pace | When the observer wakes |
| --- | --- |
| `on-stop` (default) | After each `stop` of the main agent and each `declare` |
| `on-declare` | Only at `declare` |
| `on-event <seconds>` | After new matched events, at most once per period (debounce) |
| `poll <seconds>` | At a fixed period, with the events since its cursor |

A wake is one ACP prompt into the observer's session: its role, the new events since its cursor, its messages and
their acks. The cursor moves only after the prompt ends, so a cut-off wake sends the same events again (the
observer's message ids are idempotent by `(member, cursor, index)`).

### B6. Failure and restart

| Event | Behaviour |
| --- | --- |
| Operator restart | Observer wakes are jobs; a cut-off wake loads its ACP session (`session/load`) and gets the same events again. Messages stay in the outbox; delivery is at least once, and the hook shows each message id once (`delivered` is set in the same transaction). |
| Main agent compaction | Nothing is lost: messages, counts and the thread log are in `workspace.db`; the next prompt gets the pending ones. |
| Observer crash or budget end | The member goes to `failed`; the main agent gets a `note`. Its hard blocks expire (`expired`), so a dead observer cannot lock the main agent. |
| Bad effect | `thread compensate <effect>` runs the compensation: revert the observer's commit on its topic branch, retract its message. |
| Machine restart | Threads, members and cursors are durable; observers wake at the next event. |

### B7. Branch

`thread branch <thread> --at <event>` makes a new thread with the parent's log up to that event, the same members
(new sessions), and workspaces branched from the last checkpoint at or before that event. The parent stays as it
is. Use it to try a second approach without a stop of the first.

### B8. API

New group `thread` (question 5).

| Operation | CLI | Class |
| --- | --- | --- |
| `thread.start` | `thread start <agent> "<prompt>" [--account] [--dossier]` | workspace |
| `thread.list`, `thread.show` | `thread list`, `thread show <id>` (members, messages, counts) | read |
| `thread.join`, `thread.leave` | `thread join <id> --role <role> [--pace] [--account]` | workspace |
| `thread.send`, `thread.inbox`, `thread.ack` | messages between members | session |
| `thread.declare` | `thread declare <word> [--note]` | session |
| `thread.pace` | `thread pace <member> <pace>` | session |
| `thread.rules` | `thread rules list\|add\|rm` | session |
| `thread.branch`, `thread.compensate`, `thread.import` | see B6, B7, C5 | workspace |

Each is also an MCP tool (`thread_send`, ...). The events have kind `thread.*`, so `trace` and the UI follow them.

### B9. Tests (e2e, high diagnostic value)

The observer is the stand-in ACP agent of `tests/fixtures/fake-acp-agent.py` with a role script; the hooks are the
real `legionctl hook` commands with Claude Code's hook JSON.

| Test | It catches |
| --- | --- |
| `an_observer_reminds_main_with_the_count_and_a_hard_block_stops_the_tool` | Three edits without lint give a reminder with count 3; a hard block denies `workspace_checkpoint` until ack; an expired block does not deny. |
| `an_observer_survives_an_operator_restart_and_delivers_each_message_once` | Kill the operator during a wake: the wake goes on in its loaded session; the main agent sees each message once. |
| `a_branched_thread_keeps_the_history_up_to_the_point` | The branch has the events up to `--at`, its own workspace branch, and the parent is unchanged. |
| `an_observer_writes_only_its_own_dossier_and_workspace` | Its write into the main dossier is denied; its patch arrives as a commit on its topic branch. |

## Part C: thread UI (todo-92)

### C1. What and why

When legionctl drives an account directly (a thread through ACP, C5), the person and the agents need one view of
the thread: the members, the runs, the events and the state of the task. The TUI and the app show the same layout
and the same data. Agents read the same views as text (`views_render`).

### C2. Layout

```text
┌────┬──────────────────┬──────────────────────────────────────────────┐
│ ▣  │ 10:42 main0 Edit │  Task: Two code graphs                        │
│ ◎  │ 10:43 obs0 remind│  Scope ▸ W1 ✓  W2 ▸ in progress  W3 ○         │
│ ⚑  │ 10:45 tests 71/72│  LOC  1180 / 1500 planned                     │
│ ⚙  │ 10:47 main0 ckpt │  Tests  5 linked, 4 pass, 1 no kill           │
│    │                  │                                              │
│    │                  │       ┌──────────────────────────────┐       │
│    │                  │       │ /code plan status --feature … │       │
└────┴──────────────────┴───────┴──────────────────────────────┴───────┘
 pane 1     pane 2                       pane 3 and the chat bar
```

| Element | Size | Content |
| --- | --- | --- |
| Pane 1 | 5-10% of the width | Icons: threads, members, views, the command groups of the registry, settings. |
| Pane 2 | 20-40% | Events and runs of the thread: messages, tool calls, checkpoints, test runs, jobs; filter by member. |
| Pane 3 | 50-75% | The task view for the query: scope, progress, quality, tests, changes; made of views (C4). |
| Fold | Pane 1 and pane 2 fold to one column each | TUI: one cell wide with a vertical label; app: a 24 px rail. Pane 3 takes the space. `Ctrl+1` and `Ctrl+2` fold and open. |
| Chat bar | Centred at the bottom of pane 3, 60% of its width at the start | It grows wider (drag or `Ctrl+→`) to the full width, and taller with its content to 40% of the height, then it scrolls. |

The TUI is `legionctl tui` (ratatui, the present dashboard becomes one view). The app is a new page "Threads" in
`legionctl-app` (gpui-kit). Both read the same operations and the event stream (`GET /v1/events`), so the app does
not poll for threads.

### C3. Commands

The chat bar sends text to `#main0` as an ACP prompt. A line that starts with `/` is a command:

| Source | Example | How it runs |
| --- | --- | --- |
| legionctl registry | `/code plan status --feature code-graphs` | The operation, with the CLI's words and options. |
| Thread | `/join test-reviewer`, `/branch`, `/fold 2`, `/view tests` | A `thread.*` or `views.*` operation. |
| Agent's ACP commands | `/agent:review src/` | Sent as the prompt text `/review src/` (D1). |

Completion lists all three sources, filtered by the typed prefix, with each description and input hint.

The agent's own commands:

- Through ACP: the operator stores each `available_commands_update` (today dropped in `acp.rs`) on the member and
  emits `thread.commands`.
- Without ACP (a Claude Code or Codex session in a terminal): the operator scans the command folders: plugin
  `commands/`, `~/.claude/commands`, `<repo>/.claude/commands`, user-invocable skills, and Codex's
  `~/.codex/prompts`. So "run a command on the tool to get the list" is a file scan, not a call to the harness.

### C4. Views and the UI-component agent

A view is a declarative JSONC file, not code. It binds one widget from a closed set to read operations:

```jsonc
// views/tests-of-feature.jsonc
{
  "name": "tests-of-feature",
  "params": { "feature": "string" },
  "widget": "table",                     // table | tree | gauge | sparkline | markdown | diff | list | stack
  "source": { "op": "code.plan.status", "input": { "feature": "{feature}" }, "path": "$.tests" },
  "columns": ["test", "result", "kills", "age"],
  "refresh": { "events": ["code.tests.*"] }
}
```

- Persistable: the views are config files with the config layers: user `~/.config/legionctl/views/`, repository
  `.legionctl/views/` (shared with the code).
- Recomposable: widget `stack` holds other views by name with params.
- Reusable: a view has a name and params; threads refer to views, and each thread stores its layout.
- Safe: only read operations, schema-checked by `views check`. A view cannot run a change.

The `ui-composer` observer reads the query and the progress, picks views, makes new ones (`views save`), and sets
the thread's pane 3 (`views place`). The person changes the same views by hand or with `/view`.

Operations: `views.list`, `views.show`, `views.save`, `views.check`, `views.place`, `views.render` (text for agents
and for the TUI's text widgets).

### C5. Threads per account, import

A thread belongs to one account profile (`threads.account`). A thread does not change account. `thread import <id>
--account <profile>` makes a new thread on the other account: it copies the main session's transcript (the code of
`session resume --account`), the thread log, the messages and the layout, and records `imported_from`. The original
thread stays and is read-only after the import.

### C6. Failure and restart

The UI holds no state. A UI restart reads the thread and the layout again and opens the event stream with the last
event id. A view whose operation fails shows the error in its frame; the other views stay.

### C7. Tests (e2e, high diagnostic value)

| Test | It catches |
| --- | --- |
| `the_thread_tui_folds_panes_and_runs_an_agent_command` | In a pty: fold pane 1 and 2, pane 3 takes the width; the stand-in ACP agent's `available_commands_update` gives `/agent:plan`; the prompt it gets is `/plan x`. |
| `the_app_thread_page_updates_from_the_event_stream` | Headless GPUI as in `tests/app.rs`: a `thread.message` event shows in pane 2 without a poll. |
| `a_view_cannot_bind_a_change_operation` | `views check` refuses `power.apply`; a saved view renders the same rows in the TUI text and `views render`. |
| `a_thread_imported_to_another_account_keeps_its_transcript` | The new thread runs on the other profile with the transcript; the original is read-only. |

## Part D: answers to the user's two questions

### D1. Does ACP support user-invoked commands?

Yes. In `agent-client-protocol-schema` 1.4.0 (`src/v1/client.rs`), the agent sends the session update
`AvailableCommandsUpdate { available_commands: Vec<AvailableCommand> }`; each `AvailableCommand` has `name`,
`description` and `input: Option<AvailableCommandInput>`, where `Unstructured { hint }` means "all text typed after
the command name is the input". The crate has no separate "run command" request: the client sends a normal
`session/prompt` whose text starts with `/<name>` (ACP's slash-command convention; the crate source does not state
it, and Context7 was not available on 2026-10-15 to check the protocol page). legionctl's ACP client drops this
update today (`acp.rs`, `update`: `_ => {}`). C3 stores it.

### D2. Can legionctl tools be addressed as `<toolset>/<toolname>` in searches?

No, not in Claude Code. Claude Code names a plugin's MCP tool `mcp__plugin_<plugin>_<server>__<tool>`, so
legionctl's are `mcp__plugin_legionctl_legionctl__workspace_checkpoint`. A plugin cannot change this name.
ToolSearch takes `select:<exact name>`, keywords, or `+<word>` to require a word in the name: `+legionctl
checkpoint` is the closest form to `legionctl/checkpoint`. Skills and commands of a plugin are `legionctl:<name>`;
plugin agents would be subagent types `legionctl:<role>`. In legionctl's own UI, `/` commands use the CLI words
(`/workspace checkpoint`), which map one to one to the registry name (`workspace.checkpoint`) and the MCP tool name.

## Relation to todo-78 (the CLI and app regrouping, decided last)

- todo-78 decides the final command groups. This design adds two groups only, `code` and `thread` (with `views`
  inside `thread`), so that todo-78 has fewer to move.
- Pane 1's icons and the `/` completion come from the registry's groups. A regroup in todo-78 changes the data,
  not the TUI or the app.
- todo-78 item 2 ("commands that have a live view open a TUI"): `thread` is the first group with a live view; its
  TUI is the model for the others.
- The copy of the new commands follows ASD-STE100 now, so todo-78 item 4 has less work.

## Phases

| Phase | Content | Size (new lines, estimate) | Needs |
| --- | --- | --- | --- |
| P1 | Snapshots per tree, refresh triggers, git hook, prune, `code graph refs/query/impact` | M (600) | — |
| P2 | Change graph, stable keys, content hashes, annotations, A8 skill text | M (700) | P1 |
| P3 | `planning/` format, `code plan check/status`, tokei LOC, `dossier check` findings | M (600) | P2 |
| P4 | Test runs, mutant records, review signals, checkpoint gate, pre-commit | L (900) | P3, question 2 |
| P5 | Threads, members, outbox, hook delivery, `post-tool`, watch rules and counts | L (1000) | P4 for the test rules |
| P6 | Model observers through ACP, pace, branch, compensation, role files | L (900) | P5, question 1 |
| P7 | TUI triple pane, chat bar, `/` commands, ACP commands | L (1200) | P5 |
| P8 | App thread page, views, `ui-composer`, import | L (1500) | P7 |

Each phase ends with its e2e tests, mutation checks named in the commit, `make lint` and the release test run.

## Questions for the user

1. How do observers run? Recommendation: A, operator ACP sessions with a durable outbox; the role files also serve
   as plugin subagent types.
2. Which source of mutation evidence? Recommendation: `cargo-mutants` limited to the files of the change set, and
   agent records (as today) accepted too.
3. How strong is the test gate at `workspace checkpoint`? Recommendation: refuse a new or changed test with no kill
   or no assertion; `--accept-test --reason` passes one and records why.
4. Planning folder name and format? Recommendation: `planning/` with one JSONC file per feature and `units.jsonc`.
5. Command groups for the new operations? Recommendation: two new groups, `code` and `thread`; todo-78 can move
   them later.
6. Snapshot retention? Recommendation: keep heads, bases, merge-bases, open threads' trees and the newest 20 others
   (about 1.5 GB for this repository).
7. Who checks forgotten steps? Recommendation: deterministic watch rules in the operator (no model cost); model
   observers only for judgment.
8. Build order? Recommendation: P1 to P4 (graphs and tests) first, then P5 and P6 (threads), then P7 and P8 (UI).

## Decisions (user, 2026-10-15)

The user approved this design and chose the recommendation for each question:

1. Observers run as operator ACP sessions with a durable outbox; the role files also serve as plugin subagent types.
2. Test evidence: `cargo-mutants` on the files of the change set, and the agents' mutation records.
3. Test gate at `workspace checkpoint`: refuse a new or changed test with no kill or no assertion;
   `--accept-test --reason` lets one through and records the reason.
4. Planning folder: `planning/` with one JSONC file per feature and `planning/units.jsonc`.
5. Command groups: `code` and `thread`.
6. Snapshot retention: heads, bases, merge-bases, open threads' trees and the newest 20 others.
7. Forgotten steps: deterministic watch rules in the operator; model observers only for judgment.
8. Build order: P1-P4 (graphs, planning, tests), then P5-P6 (threads, observers), then P7-P8 (UI, with todo-78).
