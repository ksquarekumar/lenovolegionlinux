# 23 — Agent network, two code graphs and the thread UI

Status: proposed (2026-10-15). Plan rows todo-90 (two code graphs), todo-91 (agent network), todo-92 (thread UI).
Part B+ (todo-97 presence, todo-98 agent configs and pools) is an addition, proposed 2026-10-15, with its own questions.
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
| B+. Configs, pools, presence (todo-97, todo-98) | Three archetypes, one team `vanilla`, one config per agent, peers | Archetype files are the lowest config layer; members are operator sessions that sleep until a trigger; the main agent's critical path comes first |
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
| see who else is in the topic / ACP sessions that share writes to common dossiers | B+1 Presence (`thread peers`) |
| send messages to them or observe their state | B+1 (`thread send --to`, `thread observe`, `SendMessage` recorded) |
| harness plugin enables agent teams, launches non-main agents as subagents, keeps them alive, sends them events | B+3 run paths, B+4 (team mode, opt-in), B+4 keep-alive |
| launched subagents visible in Claude Code / Codex like spawned subagents | B+3 visibility table |
| config tracks account, model, tools, skills, MCP, commands allow/deny/prompt per agent | B+2 Agent config |
| own-added role-specific instructions, tools, skills, composable in a pool | B+3 Composition |
| a pool has its own flow conditions on top of what an agent can | B+4 (`flow`: may, gates, stop, restart) |
| tuned and instructed to work on the dossier with dossier tooling, with examples | B+5 |
| skills not redundant or verbose; short ordered sentences; refs at declared paths | B+5 (convention, `agents/refs/<kind>-<slug>.md`) |
| main-agent, thread-observer, ui-composer are the 3 types; team `vanilla` | B+3, B+4 |
| thread-observer assumes test-reviewer, process and verifier | B+3 (three jobs of one agent) |
| process steps are automated: the observer does them and tells main; nudges for the rest | B+3 Process job |
| main agent is the load bearing worker on critical path | B+0 rule 1, B+4 Cost |
| others work less, sleep more | B+0 rule 2, B5 `on-trigger`, B+4 wake budgets |

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
| Constant only | Each assertion compares with a literal that the code under test returns as a literal (for example help text). | soft, reviewed by the thread-observer (test review job) |

Hook points:

| Point | What it does |
| --- | --- |
| `pre-tool` on `Write`/`Edit` of a test file | Static signals only (no assertion, unit test in `src/`). A note to the agent. Does not deny. |
| `thread-observer` at `stop` (test review job) | Runs the cheap signals, reads the test, and sends `soft-block` or `hint` messages (B3). |
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

Agent types: three archetypes (`main-agent`, `thread-observer`, `ui-composer`) and one team archetype that
composes them (the shipped team is `vanilla`). One `thread-observer` does the three observer jobs at once: test
review, process and verification. Part B+ has the files, the config of each agent and the pools.

How observers run (open question 1):

| Option | How | For | Against |
| --- | --- | --- | --- |
| A. Operator ACP sessions (recommended) | Each observer is a legionctl session run through its ACP adapter, with its own dossier, topic workspace and account; the operator wakes it with a prompt that holds the new events. | Durable (jobs, `session/load`), works for Claude and Codex, own sandbox and budget, runs when the main session ends, visible in `session list` and events. | Separate model cost; the real adapters are not checked live yet (14-sessions). |
| B. Claude Code subagents | Plugin `agents/<role>.md` gives subagent types `legionctl:test-reviewer` and others; the main agent starts them. | No new runtime; the main agent can call one at once. | Starts only from the main agent's turn; ends with the session; no durable state; Claude only; the main agent must remember to start it (the failure that observers must fix). |
| C. Both | The role files serve as subagent types and as the ACP observers' instructions. | One source of roles; a quick check in the main turn. | Two run paths to test. |

Recommendation: A for observers. The same archetype files also exist as plugin agents (option C at no extra cost),
because Claude Code reads `agents/` from the plugin; but only A is a member of a thread (B+3).

Deterministic watch rules (B3) run in the operator without a model. A model observer is used for judgment only.

### B2. Data model

All in `workspace.db`. The thread log is append-only (events table, `thread` column). The other tables are
projections, written in the same transaction as their event.

| Table | Columns |
| --- | --- |
| `threads` | id, title, account, root prompt, dossier, repo, parent thread, parent event (for a branch), created, state (`open`, `closed`) |
| `members` | thread, name (`main0`, `observer0`), agent config (B+2), session id, pace, cursor (last event id seen), state |
| `messages` (the outbox) | id, thread, from, to, kind, body, refs (keys, events), state (`pending`, `delivered`, `acked`, `retracted`, `expired`), created, delivered, acked |
| `watch_rules` | id, thread, member, trigger (event pattern), expect (event pattern), window (count or seconds), message kind |
| `watch_counts` | rule, count, done for main (B+3), first seen, last seen (since the member started to watch) |
| `effects` | id, member, kind (`checkpoint`, `tests.run`, `message`), ref, compensation (`revert <sha>`, `retract <msg>`), state |

### B3. Messages, watch rules and counts

| Kind | Effect on `#main0` |
| --- | --- |
| `note` | Shown in the next prompt's context. |
| `hint` | Shown in the next prompt's context, with the facts that the observer verified. |
| `reminder` | From a watch rule: "observer0: no `make lint` after 4 edits of `crates/` — 3rd time since 10:42". |
| `soft-block` | The `stop` hook stops the end of one reply with the reason (like a nudge `warn`). |
| `hard-block` | The `pre-tool` hook denies the matched tool (for example `workspace_checkpoint`) until the message is acked or retracted; the `stop` hook stops each reply. |
| `notice` | A step that the observer did for the main agent (B+3, process job), with the count of the miss. |
| `result` | A test run, a review or a patch (a commit on the observer's topic branch). |

A watch rule example: trigger `agent.tool` with `Edit` on `crates/**`, expect `agent.tool` with `Bash(make lint*)`
within the same reply, kind `reminder`. At each miss the count goes up, and the message carries the count since the
member joined. The `thread-observer` has default rules (process job): lint after a code edit, dossier write after a checkpoint, the
suite before `declare done`.

### B4. Delivery, dossiers and workspaces

Into the main agent (both Claude Code in a terminal and a session through ACP, because Claude Code runs the plugin
hooks in both; to check live with `claude-code-acp`):

| Hook | Reads | Gives |
| --- | --- | --- |
| `prompt` | pending `note`, `hint`, `reminder`, `notice`, `result` | `additionalContext`; marks them `delivered` |
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
| `on-trigger` (default, B+0) | Only at its triggers (B+3), and only when the operator's watch rules find work for a model |
| `on-stop` | After each `stop` of the main agent and each `declare` |
| `on-declare` | Only at `declare` |
| `on-event <seconds>` | After new matched events, at most once per period (debounce); opt-in per pool |
| `poll <seconds>` | At a fixed period, with the events since its cursor; opt-in per pool |

A wake is one ACP prompt into the observer's session: its instructions, the new events since its cursor, its messages and
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
| `thread.join`, `thread.leave` | `thread join <id> --agent <config> [--pace] [--account]` | workspace |
| `thread.send`, `thread.inbox`, `thread.ack` | messages between members | session |
| `thread.declare` | `thread declare <word> [--note]` | session |
| `thread.pace` | `thread pace <member> <pace>` | session |
| `thread.rules` | `thread rules list\|add\|rm` | session |
| `thread.branch`, `thread.compensate`, `thread.import` | see B6, B7, C5 | workspace |

Each is also an MCP tool (`thread_send`, ...). The events have kind `thread.*`, so `trace` and the UI follow them.

### B9. Tests (e2e, high diagnostic value)

The observer is the stand-in ACP agent of `tests/fixtures/fake-acp-agent.py` with a script per archetype; the hooks are the
real `legionctl hook` commands with Claude Code's hook JSON.

| Test | It catches |
| --- | --- |
| `an_observer_reminds_main_with_the_count_and_a_hard_block_stops_the_tool` | Three edits without lint give a reminder with count 3; a hard block denies `workspace_checkpoint` until ack; an expired block does not deny. |
| `an_observer_survives_an_operator_restart_and_delivers_each_message_once` | Kill the operator during a wake: the wake goes on in its loaded session; the main agent sees each message once. |
| `a_branched_thread_keeps_the_history_up_to_the_point` | The branch has the events up to `--at`, its own workspace branch, and the parent is unchanged. |
| `an_observer_writes_only_its_own_dossier_and_workspace` | Its write into the main dossier is denied; its patch arrives as a commit on its topic branch. |

## Part B+: agent configs, pools and presence (todo-97, todo-98)

Status: proposed (2026-10-15), an addition to the approved Part B. The user's words are in the dossier
(`MEMORY/11-agent-network-and-code-graphs.md`, "Addition" and "Archetypes"). The harness facts are in
`MEMORY/12-research-agent-teams-2026-10-15.md`; "to check" marks a fact that comes from a search snippet or is not
documented, and that a live check must confirm before its phase starts.

Binding: three agent archetypes (`main-agent`, `thread-observer`, `ui-composer`); one team archetype that composes
them, shipped as the team `vanilla`; one `thread-observer` does the three observer jobs (test review, process,
verification); observers run as operator ACP sessions with a durable outbox; watch rules run in the operator.

### B+0. Two rules of the pool (user, 2026-10-15)

1. "Main agent is the load bearing worker on critical path." The `main-agent` does the critical-path work. The
   `thread-observer` and the `ui-composer` take work off it and keep it correct; they never slow it:
   - they work off the critical path: in their own context, on their own topic branch, or in the main agent's idle
     windows; the main agent never waits on a lock that a member holds;
   - their steps for the main agent never touch a file that the main agent has changed since its last checkpoint
     or has open in the current reply (B+3, process job);
   - nudges are short, batched and timed: they arrive at the next prompt after a `stop` or `declare`, as one block;
     only a hard fault arrives at once;
   - `hard-block` is only for a fault that makes the main agent's result wrong (for example a weak test that is
     about to be committed, a checkpoint with a failing suite); all else is `soft-block` or a nudge;
   - the cost to the main agent is measured and shown (B+4): the wait its hooks add and the tokens of the context
     they inject.
2. "Others work less sleep more." The `thread-observer` and the `ui-composer` sleep by default (pace `on-trigger`,
   B5). While they sleep, the operator's watch rules (no model) keep the counts and decide if a wake is worth a
   model call. Each wake has a budget (tokens and time). Watching or polling is an opt-in per pool.

### B+1. Presence (todo-97)

An agent sees its peers, their state, and sends to them or observes them. A peer is a session that shares one of
these with the asking session:

| Shared | Source (all in `workspace.db`, nothing new to collect) |
| --- | --- |
| the topic workspace | the workspace row of each session (18-workspaces) |
| a dossier it writes | the dossier binding of each session (`session start --dossier`, the hooks' session id) |
| a thread | the `members` table (B2) |

`thread.peers` joins the three and adds the live state of each peer:

| Field | From |
| --- | --- |
| `session`, `agent` (claude, codex), `account`, `agent config` (B+2) | the session record |
| `state`: `running`, `idle`, `paused`, `offloaded`, `failed`, `ended` | the systemd scope (14-sessions) and the last hook event (`prompt` = running, `stop` = idle) |
| `last` (time, tool, path) and `declared` (the last `thread declare` word) | the newest `agent.tool` and `thread.declare` events |
| `pending` | the count of outbox messages to the peer that are not `acked` |
| `channel`: `outbox`, plus `claude-inbox` | `claude-inbox` when the peer is a Claude Code session that was not started with `--bare` (it binds an inbox socket, so `ListAgents` shows it) |

Send: `thread.send --to <peer>` writes the outbox (B2), which is the one legionctl path for all peers. The receiver
gets it as in B4 (its hooks) or, for an operator member, in its next wake (B5). An idle Claude Code peer in a
terminal gets it at its next prompt; to reach it at once, a Claude Code sender uses the harness's own `SendMessage`
(cross-session messaging; the receiver's `crossSessionInbound` decides accept, hold or refuse). legionctl does not
send through the inbox socket. The `post-tool` hook records each `SendMessage` call as a `thread.message` event, so
the thread log stays complete (question 1).

Rules: any peer can send `note` and `hint`. Only a member of the same thread with the `thread-observer` archetype
can send `soft-block` and `hard-block` (B3), so a peer from another thread cannot stop the main agent.

Observe: `thread.observe <peer> [--follow]` gives the peer's events (the same rows as `trace`); `--follow` streams
them. It reads only; to watch with rules, join the thread (`thread join`).

| Operation | CLI | MCP tool | Class |
| --- | --- | --- | --- |
| `thread.peers` | `thread peers [--dossier <d>] [--topic <t>] [--thread <id>]` (default: the caller's own) | `thread_peers` | read |
| `thread.observe` | `thread observe <peer> [--follow] [--kinds <k,...>]` | `thread_observe` | read |
| `thread.send` (B8, `--to` now also takes a peer that is not a member) | `thread send --to <peer> <kind> "<text>"` | `thread_send` | session |

### B+2. Agent config (todo-98)

One config per agent. It has the fields below, and it resolves in layers like all legionctl config (14-sessions,
"Config model"):

1. The archetype file (B+3) is the lowest layer: its frontmatter and its instruction text.
2. The key `agents.configs.<name>` in the config layers is above it: defaults, `/etc/legionctl/`, the user's
   `~/.config/legionctl/`, the repository's `.legionctl/`, the session. The repository layer may only narrow
   (fewer tools, fewer commands, a smaller budget), as the plugin allowlist does (12-operator); `/etc` can lock a
   key.
3. The team's flow (B+4) is the top layer, and it may only narrow.

`legionctl config explain agents.configs.observer.model` shows each layer that set the field, the archetype file
first. A session stores its resolved config (14-sessions), so an edit changes a running member only at its next
start or resume.

```jsonc
// ~/.config/legionctl/agents.jsonc
{
  "agents": { "configs": {
    "observer": {
      "archetype": "thread-observer",
      "harness": "claude",                 // claude | codex
      "account": "work2",                  // an `agents accounts` profile
      "model": "sonnet", "effort": "medium",
      "tools": { "deny+": ["WebFetch"] },
      "skills+": ["verify-and-ship"],
      "mcp": ["legionctl", "ctx"],         // names in `mcp.servers`, through the proxy
      "commands": { "deny+": ["Bash(cargo publish*)"], "ask+": ["Bash(cargo mutants*)"] },
      "instructions+": "integrations/agent-plugin/agents/refs/reference-observer-local-rules.md",
      "pace": "on-trigger",
      "budget": { "tokens_per_day": 2000000 }
    }
  } }
}
```

How the operator applies each field. The plugin path cannot carry four fields (plugin agents ignore
`permissionMode`, `hooks`, `mcpServers` and `initialPrompt`), and Codex subagents inherit sandbox, approval and MCP.
So the operator sets these fields itself for each session it starts:

| Field | Claude Code, operator session | Claude Code, plugin subagent of the main agent | Codex, operator session | Codex subagent (`~/.codex/agents`) |
| --- | --- | --- | --- | --- |
| instructions | `--agent legionctl:<archetype>` (to check) and the first ACP prompt | the archetype file's text | `developer_instructions` in the member's own `CODEX_HOME` agent file, and the first prompt | `developer_instructions` |
| account | `CLAUDE_CONFIG_DIR` of the profile | the main agent's account | `CODEX_HOME` of the profile | the main agent's |
| model, effort | `--model`; `effort` from the frontmatter, or the settings file the operator writes | frontmatter `model`, `effort` | `-c model=`, `-c model_reasoning_effort=` | the TOML fields |
| tools allow and deny | `permissions.allow/deny` in the session's `claude-settings.json` (S2a) and frontmatter `tools`/`disallowedTools` | frontmatter `tools`/`disallowedTools` | sandbox mode and approval policy (`-c`) | inherited: cannot narrow |
| skills | `session.skills` (the sandbox shows only these) and frontmatter `skills` | frontmatter `skills` (preload) | `session.skills` | `skills.config` |
| MCP | `mcp.json` through the proxy, `--strict-mcp-config` (S2b) | the main agent's servers; `disallowedTools` hides tools | `mcp_servers` in the member's `CODEX_HOME` | inherited: cannot narrow |
| commands allow, deny, ask | `permissions.allow/deny/ask` in the settings file, and the git and command shims (layer 2 of the gates) | the main agent's permissions; the `pre-tool` hook adds the agent's own deny list if the hook input names the subagent's type (to check), else the config is refused for this path | approval policy and the shims | inherited |
| env | `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` for a team-mode lead (B+4), `LEGIONCTL_MEMBER=<thread>/<member>` | — | `LEGIONCTL_MEMBER` | — |
| pace, budget | the operator (B5, 19-gateway) | — (lives only in the main turn) | the operator | — |

Rule: a field that a run path cannot apply is not dropped in silence. `thread join` and `session start` refuse the
config for that path and name the field; `agents check` (B+3) lists such fields for each file.

### B+3. Archetypes, files and run paths

Three archetype files, each one source for both run paths (approved option C):

| File | Archetype | Does | May write | Message kinds it may send |
| --- | --- | --- | --- | --- |
| `integrations/agent-plugin/agents/main-agent.md` | `main-agent` | Owns the task and its completion; acks messages; declares states. | its topic workspace, its dossier | `note` |
| `integrations/agent-plugin/agents/thread-observer.md` | `thread-observer` | The three jobs below, in one context. | its own dossier `<main>--<member>`, its own topic workspace | all of B3 |
| `integrations/agent-plugin/agents/ui-composer.md` | `ui-composer` | Makes and places views (C4). | the thread's views | `note`, `result` |

The `thread-observer`'s three jobs. One wake handles all three. Its triggers (pace `on-trigger`) are the union
below, and a trigger wakes it only when an operator watch rule finds work that needs a model:

| Job | Triggers | It does |
| --- | --- | --- |
| Test review | a change to a test file in the change set (`agent.tool`); a `code tests` result | runs `code tests review` and mutation runs; sends `hint`, or `hard-block` for a weak test that is about to be committed (A7) |
| Process | the main agent's `stop`; a checkpoint; a hard fault from a watch rule | does the steps it can do and sends a `notice`; sends a nudge for the others (table below) |
| Verification | `declare done`, `declare ready` | runs the suite and `code plan status`; sends a `result` that names each open item, and a `hard-block` on `workspace_checkpoint` when the suite fails |

The `ui-composer`'s triggers: a view request (`/view`, `views place`), and the main agent's `stop` while a TUI or
the app shows the thread.

Process job: "tool calls, dossier writes, checkpoints are automated and observer can do it and tell main agents,
other things it sends nudges to main agent instead" (user, 2026-10-15). A step that the observer can do itself, it
does, then it sends a `notice` (B3). All else is a nudge (`reminder` or `hint`):

| Missed step | Done by the observer, with | Where, without a race |
| --- | --- | --- |
| lint | the repository's lint command (`make lint` here), check mode only | an overlay of the main agent's workspace (14-sessions: read-only lower layer, the observer's own upper layer), so the main agent's files do not change |
| a targeted test run | `code tests run --changed` | the same overlay; the run's budget is the wake's |
| a graph refresh, `dossier check` | `code graph refresh`, `dossier_check` | read operations; no race |
| the dossier record of a checkpoint | `dossier_append` of a ledger line marked `by observer0` | the main agent's dossier, append only, through the dossier lock, only while the main agent is `idle` (question 5) |
| a checkpoint | `workspace checkpoint` with the same test gate (A7) | the main agent's topic branch, only while the main agent is `idle`: a commit from a private index (`commit-tree`) and an `update-ref` that compares the old value; if the main agent wrote in the meantime, the update fails and the observer tries at the next idle window. The main agent's index and files do not change. |

Nudge, not a step, for: a lint or test failure (the fix touches the main agent's files), a change to a file that the
main agent changed since its last checkpoint, a judgment (design, scope, test value), and a step that the observer's
config denies. Counts stay as in B3: a watch rule counts the main agent's miss whether the observer did the step
or not; `watch_counts.done for main` counts the steps the observer did. A `notice` reads: "observer0: ran `make
lint` after 4 edits of `crates/` (clean) — 3rd miss since 10:42, 2 done for you".

Frontmatter: only fields that the plugin path honours (`name`, `description`, `tools`, `disallowedTools`, `model`,
`effort`, `skills`, `background`). The operator-only fields (B+2) live in `agents.configs` defaults, compiled into
the binary with `config/defaults.jsonc`, with one entry per archetype. No field is in both places.

Composition: one agent = one archetype file + its config layers (B+2). A config may:

- change `account`, `model`, `effort`, `pace`, `budget`;
- add skills, MCP servers (from the `mcp.servers` allowlist), instruction refs (`instructions+`), and watch rules;
- narrow tools and commands.

A config may not change the archetype's write area or its message kinds, and may not add tools or commands that
the archetype denies. `legionctl agents check` (run by `make static`) refuses a config or team that widens, and a
reference to a ref file that does not exist.

Install and visibility:

- Claude Code: the plugin ships `agents/*.md`, so the subagent types are `legionctl:main-agent`,
  `legionctl:thread-observer`, `legionctl:ui-composer`. Claude Code must read `agents/` without its subfolders, as
  `agents/refs/` is not an agent folder (to check; if it reads subfolders, refs move to `agent-refs/`).
- Codex: `legionctl agents install --agent codex [--scope user|project]` writes `~/.codex/agents/<archetype>.toml`
  (or `.codex/agents/`) from the same files, through the `agents set` writer (diff, backup, rename).

| Where | Operator member (thread member) | Plugin subagent | Claude Code teammate (team mode, B+4) | Codex subagent |
| --- | --- | --- | --- | --- |
| `thread show`, `thread peers`, TUI pane 2 | yes | yes, from the `SubagentStart`/`SubagentStop` hooks | yes, from the hooks | yes if Codex hooks run (to check) |
| Claude Code `/tasks`, background panel | no (another process) | yes | the team panel or tmux panes | — |
| Claude Code `ListAgents` | yes, when not `--bare` | — | yes | — |
| Codex agent list | — | — | — | yes |
| Durable (survives restart, compaction, `/resume`) | yes (B6) | no; a background one resumes by `SendMessage` while its session lives | no: not restored after `/resume` | no |

Limits from the research that shape this: one team per Claude Code session, no nested teams, a fixed lead,
teammates only in an interactive session (in `-p` they run as plain subagents), and in-process teammates are not
restored after `/resume`. So the thread's members are operator sessions; the other paths are for a quick check in
the main turn and for visibility.

### B+4. Pools: the team archetype and `vanilla`

A pool is a thread's members plus flow conditions. A team file defines a pool; later teams are more files of the
same form. Paths: shipped `integrations/agent-plugin/teams/<team>.jsonc` (next to `agents/`; Claude Code does not
read `teams/`), the user's `~/.config/legionctl/teams/`, the repository's `.legionctl/teams/` (narrow only, as
B+2). `legionctl thread start --team vanilla "<prompt>"` starts a thread with its members.

```jsonc
// integrations/agent-plugin/teams/vanilla.jsonc
{
  "name": "vanilla",
  "members": {
    "main0":     { "agent": "main-agent" },
    "observer0": { "agent": "thread-observer", "pace": "on-trigger",
                   "wake_budget": { "tokens": 40000, "seconds": 300 } },
    "ui0":       { "agent": "ui-composer", "pace": "on-trigger", "when": "ui",  // only when a TUI or app shows the thread
                   "wake_budget": { "tokens": 15000, "seconds": 60 } }
  },
  "flow": {
    "may": { "ui0": ["note", "result"] },                // narrows the archetype's kinds
    "gates": {
      "checkpoint": ["observer0: no open hard-block"],
      "declare:done": ["observer0: result verification ok"]
    },
    "stop": ["main0 ended and idle 30m", "declare:done acked", "budget"],
    "restart": { "max": 3, "within": "1h" }
  }
}
```

Each `members` entry names an agent config (B+2) by name, or an archetype for its defaults; a member may also set
`account`, `model`, `pace` (narrow only for tools and commands).

How events reach each member and how it answers:

| Member runs as | Events in | Out |
| --- | --- | --- |
| `main-agent` in a terminal or ACP | hooks `prompt`, `pre-tool`, `stop` (B4) | `thread ack`, `thread declare`, `thread send` |
| operator member (default) | its wake prompt: the events since its cursor (B5) | `thread_send` (MCP tool) into the outbox |
| Claude Code teammate (team mode) | the `TeammateIdle` hook answers exit 2 with its pending outbox messages, so the teammate keeps working on them; `TaskCompleted` exit 2 holds a task until the pool's gate passes | `thread_send`; `SendMessage` is recorded by `post-tool` |
| plugin subagent | `SubagentStop` blocks with the pending messages as its reason | `thread_send` |

Triggers come from the archetype (B+3); a team file can narrow them, or opt in to `on-event` or `poll` (B5) for
one member. A wake that reaches its `wake_budget` ends; its events stay after the cursor for the next wake.

Cost (rule 1 and 2 of B+0), recorded per thread and shown by `thread show`, `thread peers` and the TUI's pane 2:

| Measure | From |
| --- | --- |
| added wait of the main agent | the time each `prompt`, `pre-tool` and `stop` hook spends on thread work, and the time a gate holds a tool |
| injected tokens into the main agent | the size of each `additionalContext` that the hooks give (counted as tokens with the gateway's counter, 19-gateway) |
| each member's tokens and time | per wake, from the member's session usage (`agents usage`) |
| observers' total next to the main agent's | the sum per thread, as a ratio |

Gates: `gates.checkpoint` is checked by the `pre-tool` hook (as a `hard-block`) and by `workspace checkpoint`
itself; `gates.declare:done` makes `thread declare done` return the open items instead of a done state.

Keep-alive and restart: an operator member is a job (B6); at a crash or budget end it goes to `failed`, and the
pool restarts it within `restart` (`session/load` first, a new session with the thread log second). Past the limit
it stays `failed`, its hard blocks expire, and the main agent gets a `note`. A teammate lost at `/resume` (the
`SessionStart` hook with source `resume` sees the team without it) is recorded as `lost` and restarted as an
operator member. A pool ends at its first `stop` condition; its members end, its log stays.

Team mode (Claude Code agent teams) is not the default. The operator can set `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`
only for a lead it starts (a plugin's `settings.json` cannot set env), and the limits in B+3 apply. A team file may
set `"run": "claude-team"` after the live check (question 4).

### B+5. Dossier tooling, instruction text and refs

- Each archetype file tells the agent to use the dossier tools, in the order of its work. The rules of the dossier
  stay in one place, the `dossier-practice` skill; the archetype file names the skill in `skills` and does not
  repeat it.
- The instruction text follows the skill convention (memory `skill-writing-convention`): short ordered sentences
  that build on each other, each rule once, no conflict with a skill or another archetype. `agents check` is the
  place where todo-99's skill check also reads the archetype files.
- Examples, scenarios and references are ref files, not instruction text: `integrations/agent-plugin/agents/refs/
  <kind>-<slug>.md`, `kind` = `example`, `scenario` or `reference`, as for skills
  (`integrations/agent-plugin/skills/<skill>/refs/<kind>-<slug>.md`). The instruction text names each ref by its
  path; `agents check` refuses a missing one.

The start of `thread-observer.md` (a sketch of the form, not the final text):

```markdown
1. #main0 owns the task and works on the critical path. You keep it correct and never slow it.
2. You wake only on a trigger. Each wake gives the events since your cursor; act only on them, within the budget.
3. Read the dossier of #main0; write your own dossier (skill dossier-practice).
4. Test review: read each changed test and its kill record. Weak test: hint; about to be committed: hard-block.
5. Process: do each missed step that you can do, while #main0 is idle, then send a notice. Nudge for the rest.
6. Verification: at "done" or "ready", run the suite and `code plan status`. Name each open item in a result.
7. Each message is short and names the fact you verified and how #main0 clears it.
Refs: agents/refs/example-observer-hard-block-weak-test.md, agents/refs/scenario-observer-declare-done-fails.md.
```

Shipped refs (first set): `example-main-ack-and-declare`, `example-observer-hard-block-weak-test`,
`scenario-observer-declare-done-fails`, `scenario-peer-message-to-idle-session`, `reference-message-kinds`,
`example-ui-composer-place-tests-view`.

### B+6. Phases and tests

P5 grows by presence (B+1, about 300 lines). P6 splits; the order stays P5, then P6:

| Phase | Content | Size (new lines, estimate) | Needs |
| --- | --- | --- | --- |
| P6a | Archetype files, refs, `agents.configs` resolution, per-harness application (B+2 table), `agents check`, `agents install --agent codex` | M (700) | P5 |
| P6b | Operator members through ACP, pace `on-trigger` and wake budgets, branch, compensation, team files, `vanilla`, flow (may, gates, stop, restart), the process job's steps (overlay, private-index checkpoint), cost measures | L (1100) | P6a |
| P6c | Team mode (`run: claude-team`): `TeammateIdle`/`TaskCompleted` delivery and gates, lost teammates | S (300) | P6b, question 4, live check |

Tests (e2e, high diagnostic value; the stand-ins of B9, and stand-in `claude` and `codex` commands that record
their argv, env and the files they get):

| Test | It catches |
| --- | --- |
| `peers_show_the_sessions_on_the_dossier_with_their_state_and_a_message_reaches_an_idle_peer_once` | A session on another dossier shown; a stale state after `stop`; a message delivered twice or never; a peer from another thread able to hard-block. |
| `each_config_field_reaches_the_harness_or_the_config_is_refused` | A field dropped in silence on a path that ignores it (for example `mcp` on a plugin subagent, tools on a Codex subagent); the wrong account folder; the teams env var set on a session that is not a lead. |
| `a_repository_config_or_team_that_widens_is_refused_and_explain_names_the_file` | The repository layer adding a tool or a message kind; `config explain` not showing the archetype file as the lowest layer. |
| `vanilla_wakes_the_observer_on_its_events_and_the_gates_hold_checkpoint_and_done` | A wake missed for `declare done` or a test edit; checkpoint passes with an open hard block; done passes with a failed verification. |
| `the_observer_sleeps_until_a_rule_finds_work_and_a_wake_stops_at_its_budget` | A model wake for a `stop` with nothing to do; a wake that runs past its token or time budget; events lost after a cut-off wake. |
| `the_observer_does_a_missed_step_without_touching_main_work_and_counts_it` | Lint run in the main workspace (a file changes); a checkpoint while main is running, or one that wins over a main commit made at the same time (the compare must fail); a step done on a file main changed (must be a nudge); the miss not counted because the observer did it. |
| `the_cost_to_main_is_measured_and_shown_next_to_the_observers_cost` | Hook wait or injected tokens not recorded; nudges given one by one instead of as one block at the next prompt. |
| `a_failed_member_restarts_within_its_limit_then_its_blocks_expire` | A restart loop past `max`; a dead observer that still blocks the main agent. |
| `a_teammate_gets_its_messages_through_the_idle_hook_and_a_lost_one_restarts_as_a_member` (P6c) | Messages lost for a teammate; a teammate lost at `/resume` and not seen. |

### B+7. Questions for the user

1. Messages between Claude Code peers: is the outbox the only legionctl path? Recommendation: yes. legionctl sends
   only through the outbox; a Claude Code agent may still use the harness's `SendMessage` for an urgent message to
   an idle peer, and the `post-tool` hook records it in the thread log.
2. Where does an agent config live? Recommendation: the archetype file is the lowest layer; `agents.configs.<name>`
   in the legionctl config layers is above it; the repository layer and the team flow may only narrow.
3. A config field that a run path cannot apply (for example per-agent MCP on a plugin subagent): drop it with a
   warning, or refuse? Recommendation: refuse and name the field, so a policy never looks applied when it is not.
4. Claude Code agent teams: when do we use them? Recommendation: `vanilla` runs its members as operator sessions;
   team mode is an opt-in per team file (`run: claude-team`) in P6c, after a live check of the teammate hooks.
5. The process job writes the missing dossier record. Approved B4 says an observer writes only its own dossier.
   Recommendation: allow one exception: an append-only ledger line marked `by <member>` in the main agent's dossier,
   through the dossier lock, only while the main agent is idle; all other writes stay in the observer's dossier.
6. Codex members: Codex subagents inherit sandbox, approval and MCP. Recommendation: a Codex member of a pool always
   runs as its own operator session; `~/.codex/agents` files are installed only for quick use in a Codex session,
   with instructions, model and effort.

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
| Thread | `/join thread-observer`, `/branch`, `/fold 2`, `/view tests` | A `thread.*` or `views.*` operation. |
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

The `ui-composer` member reads the query and the progress, picks views, makes new ones (`views save`), and sets
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
plugin agents are subagent types `legionctl:<archetype>` (B+3). In legionctl's own UI, `/` commands use the CLI words
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
| P5 | Threads, members, outbox, hook delivery, `post-tool`, watch rules and counts, presence (B+1) | L (1300) | P4 for the test rules |
| P6 | Model observers through ACP, pace, branch, compensation; agent configs, archetypes, pools (split in B+6) | L (1700) | P5, question 1 |
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
