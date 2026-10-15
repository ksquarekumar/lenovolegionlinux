# 23 — Agent network, two code graphs and the thread UI

Status: approved (2026-10-15, "Decisions" at the end). Plan rows todo-90 (two code graphs), todo-91 (agent
network), todo-92 (thread UI). Part B+ (todo-97 presence, todo-98 agent configs and pools) was added and approved on
2026-10-15 ("Decisions on Part B+"). Part E (todo-100..todo-104: project checks, test runners, graph schema, change
watcher, dossier home, observer recovery) was added and approved on 2026-10-15 ("Decisions on Part E"), at the end. Built: P1 and P2 (section "Built: P1 and P2", after A11), P3 (section "Built: P3"), P5 (section "Built: P5", after B+7), P10 (section "Built: P10", after E2). The user's request (verbatim) and the decisions are in the
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
| E. Checks, runners, schema, watcher, home, recovery (todo-100..104) | Repository checks, three test sets, graph rules and queries, a change watcher, dossier homes, the recovery brief | Checks and queries are repository config; the root daemon watches with fanotify and the user's operator records; each dossier's SQLite is in `~/agentWorkspaces/<name>` |
| C. Thread UI (todo-92) | Triple pane, chat bar, `/` commands, views | One layout in the TUI (ratatui) and the app (GPUI); views are declarative JSONC from a closed widget set |

Each part has: what, why, data model, API, hooks, storage, failure and restart behaviour, tests and phases. The
questions for the user are at the end.

## Coverage of the request

Each phrase of the user's request maps to a section. "Open" means a question at the end. Part E's phrases are in
E10.

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
`cargo test --profile test-fast --workspace --no-fail-fast -j8`. The job reads the libtest lines
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

#### Built: P4 without the planning links (2026-10-15)

What exists (`crates/legionctl-core/src/codetests.rs`, operations `code.tests.*`, `workspace.db` v12):

| Part | Built | Different from the design, and why |
| --- | --- | --- |
| Runs | `code tests run [--filter]`: a job (`code.tests.run`, the counts so far are its progress). Each test is a row in `test_runs`: repository, ref, file (the source of the test binary, from cargo's `Running` line), test, result, job, time. | No time per test: stable libtest does not print it. The command is fixed (`cargo test --profile test-fast --workspace --no-fail-fast -j8`), not a `tests.command` setting. |
| Mutants | Table `mutants` (repository, ref, file, line, description, killed = the tests that failed, source `tool` or `agent`). `code tests mutant record <file> <line> <description> [<test>...]` and `code tests mutant run` (cargo-mutants on the `.rs` files outside `tests/` that the branch changed since its base; a job with `done`/`total` progress; the tests that failed come from each mutant's log). | Records count for the repository (git common folder), not for one tree: a test name with a kill is evidence until the test changes again. |
| Review | `code tests review [--staged]`: the `#[test]` functions that the diff against HEAD touches. Hard: no assertion, no kill. Soft: duplicate (another test kills each of its mutants), outside `tests/`. | The spans and the boundary come from the text, not from the code graph: a snapshot exists only for a committed tree and needs ctx, and the gate reviews the index or the working tree (CI has no ctx). Braces in strings can cut a span; "boundary" is "in a `tests/` folder", not the callees. No "constant only" signal (the test-reviewer observer, P6). |
| Gate | `workspace checkpoint` stages, reviews the index and refuses with each test and reason; nothing is committed. `--accept-test <test> --reason <why>` keeps the reason in `test_accepts` and writes the event `code.tests.accept`. | One test per accept; an accept stays, so the pre-commit hook and later checkpoints see it. The annotation of the reason waits for A6 (P2). |
| Pre-commit | Hook `legionctl-test-gate` (`scripts/test-gate.sh`): `code tests review --staged` when something is staged. | When the `legionctl` on `PATH` has no `code tests review` (a package older than the repository), the hook refuses the commit: install the new package (`make package install-package`). The user's decision, 2026-10-15. |
| Not built | Links from tests to stories and work items (needs P3), the `pre-tool` note on test files and the observer (P5/P6). | |

Tests: `the_checkpoint_gate_refuses_tests_without_evidence_and_takes_kills_and_accepts`,
`a_test_run_records_each_test_with_its_file_result_and_ref` (stand-ins for cargo-mutants and cargo that write their
real output), and `cargo_mutants_on_a_fixture_crate_records_the_tests_that_kill_its_mutants` (`#[ignore]`: the real
cargo-mutants 27.1, run locally).

### A8. Rebase and conflicts

The workspace-practice skill gets a section "Rebase and conflicts":

1. Before you resolve a conflict, run `code graph changes --three-way <merge-base> <ours> <theirs>` (todo-119): base to
   ours, base to theirs, and the entities that both sides changed.
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

### Built: P1 and P2 (2026-10-15)

| Part | Where |
| --- | --- |
| Snapshots, refs, prune, git hook, change graph, annotations | `crates/legionctl-core/src/codegraph.rs` |
| Jobs and triggers (checkpoint, merge, session start) | `crates/legionctl-operator/src/code.rs`, one line each in `engine.rs` |
| Table `annotations` | `workspace.db` v11 (`crates/legionctl-core/src/dossier.rs`) |
| Operations | `code.graph.refresh`, `refs`, `query`, `impact`, `changes`, `annotate`, `annotations`, `prune`, `hook` (CLI `code graph <name>`, MCP `code_graph_<name>`) |
| Skill text (A8) | workspace-practice ("Code graphs of the refs", "Rebase and conflicts"), verify-and-ship ("Before a merge or after a rebase"), tool-lifecycle (group `code`) |

What is different from the sections above, and why:

| Item | Built | Why |
| --- | --- | --- |
| Stable key (A3) | ctx's `binding_key` column first; else the first binding alias that is not a package alias and not a bare `symbol:<name>`; else the node id. Then each `@<byte offset>` is removed. | ctx 2.2.8 gives a private function only the aliases `symbol:<name>` and `symbol:<file>::<name>`; `symbol:<name>` is not unique in a repository. `binding_key` has the module path for private items too. Closures and blocks have offsets in their aliases (`<closure@85>`). Measured on this repository, HEAD~3 to HEAD: 173 keys added, 0 removed; the only keys that two nodes share are closures in one function (they get `#2`, `#3` in file order). |
| Modules and files | A module has its `binding_key` (`rust:module:<crate dir>:<path>`). Each changed file is a row `file:<path>` from `git diff --numstat`. | A file and its module are two entities: a module can move to another file (`util.rs` to `util/mod.rs` is `moved`). |
| `code graph changes` | Refs are positional: `changes a b c` gives a to b and b to c; one ref: from its first parent; none: HEAD's last commit. `--three-way <base> <ours> <theirs>` (todo-119) gives base to ours and base to theirs; the second set has `both`: the keys that both sides changed, without a file or module row when a narrower entity of its file changed. The text adds "changed on both sides" with the change and commits of each side. | The rebase guidance compares more than two refs. In a row, ours to theirs shows the new work of ours as removed on a rebase. A file or module that both sides changed is position only when a function of it says what changed. |
| Commits of an entity | Added or modified symbol: `git log -L` of its lines at the end of the range. Removed or moved symbol: `git log -S<its text> --no-renames`. File, and a node of the whole file: the log of the file. | A move changes no lines, and `-L` does not see lines that are gone. |
| Checkpoint event of a commit | The newest `workspace.checkpoint` event with the commit's message. | The event log keeps the input of a call, not its output. |
| When triggers act | Only in a repository that keeps graphs: its `.graf/refs` exists (the first `code graph refresh` or `code graph hook install` makes it). | legionctl does not write `.graf` into a repository that did not ask for it, and the tests of other features see no ctx jobs. |
| Ignore `.graf/` | `/.graf/` in `<common dir>/info/exclude` when git does not ignore it yet. | One file for all worktrees, and no commit on the branch. |
| ctx's MCP server | Each checkout's `.graf/index.db` is a link to the graph of its HEAD (set by each refresh of HEAD). `ctx mcp serve` opens the nearest `.graf/index.db`, so its config does not change. A graph file is read-only (0444), so a `ctx graph index` through the link cannot change it. | The old `index.db` of the main checkout (2026-10-12) is replaced by the link at the first refresh there. |
| Concurrency | `ctx graph index` into `<tree>.db.tmp.<pid>.<ns>`, then a hard link to `<tree>.db`: the first builder wins; the second removes its file. | `rename` would replace the file of the first builder. |

Not built yet: `--to work` (the working copy), the change-set cache `.graf/changes/`, `--impact` on `changes`,
annotations on event keys (`commit:`, `event:`, `job:`), the ctx version of each graph (the `metadata` table of ctx
2.2.8 has none), and the trees of open threads in prune (threads come in P5). The session-start trigger has no test
of its own (the checkpoint and git-hook triggers have).

Tests (`crates/legionctl/tests/code.rs`): ctx is a stand-in (`tests/fixtures/ctx-standin`, Python and SQLite) that
writes the `index.db` schema of ctx 2.2.8 and its node shape for free functions, private functions, closures and
call edges. `the_real_ctx_gives_the_change_rows_of_the_stand_in` (ignored, run by hand) runs the same change
scenario with the installed ctx and expects the same rows.

| Test | It catches |
| --- | --- |
| `a_graph_is_made_once_per_tree_shared_by_worktrees_and_pruned_to_heads_bases_and_the_newest` | No graph before the repository keeps graphs; a checkpoint and a plain `git commit` (the hook) make the graph of their tree in the main checkout's `.graf`; each checkout's link follows its HEAD (also after a checkout); two refreshes of one tree leave one file and no temporary files; a ref without a graph is reported and refused, and `--build` makes it; prune keeps a merge-base that is older than the removed graphs, and the newest 20 others. |
| `changes_between_refs_are_the_changed_entities_by_stable_key_with_their_commits` | Exactly the changed entities with their commits; a function whose offsets moved and an unchanged closure are not changes; a function in a moved file of the same module is `moved`; checkpoint event, session and plan row of each commit; consecutive change sets; impact by stable key at a ref. |
| `an_annotation_is_valid_while_its_symbol_is_the_same_stale_after_a_change_and_absent_after_removal` | `valid` with other offsets, `stale` after a body change, `absent` after removal, `no graph` at a ref without one. |

### Built: P3 (2026-10-15)

| Part | Where |
| --- | --- |
| Format, `code plan check`, `code plan status`, `dossier check` findings | `crates/legionctl-core/src/planning.rs` |
| Schemas | `planning/feature.schema.json`, `planning/units.schema.json` (written by the wire-format test) |
| Operations | `code.plan.check`, `code.plan.status` (CLI `code plan <name>`, MCP `code_plan_<name>`), class read |
| The first plan of this repository | `planning/features/` (code-graphs, agent-network, thread-ui, sensors-events, kernels), `planning/units.jsonc` |
| Skill text | workspace-practice (rule 17, `refs/reference-planning.md`), tool-lifecycle (group `code plan`), verify-and-ship (rule 11) |

What is different from A4 and A5, and why:

| Item | Built | Why |
| --- | --- | --- |
| Work item | Has `text`. Its tests are the tests of its stories. | A row without text says nothing in `plan status`. |
| Unknown keys | Each planning type refuses an unknown field. | A typo in a field name must not pass as an empty list. |
| Key prefix `symbol:` | Added. It resolves against the graph as `rust:` does. | ctx 2.2.8 gives a method no `rust:` key, only `symbol:<file>::impl <type>.<method>`. |
| `config:` and `op:` | Resolved against `config/defaults.jsonc` and `docs/api/schema.json` at the tree. | The tree is the source; `legionctl validate` reads the installed config, not the tree. |
| When a missing unit is a problem | `check` refuses an `edit` unit that is not there, and each unit or test of a work item or feature with status `done` that is not where its change puts it. An `add` unit of an open item may be missing. | The plan is written before the code. |
| Complete | Each unit is where its change puts it, and each linked test is in the graph and did not fail at its last recorded run (`test_runs` of P4; a test with no run counts). Each test shows its last result and its recorded kills. | "Changed since the feature's start" (A5) is not built: a feature has no start ref yet. |
| LOC | tokei 15 `-o json` (its `code` count) of the files of `file:` and `unit:` units, read from the blobs of the tree in a temporary folder; the span for `rust:`, `symbol:` and `test:` units. Without tokei, a note and no LOC. | tokei counts files only. The blobs make LOC of another ref correct. |
| tokei | `~/.cargo/bin/tokei`, else `PATH`. | The operator's `PATH` does not have `~/.cargo/bin`. |
| Changes since a ref | `--since <ref>`: the entities of `code graph changes <since> <ref>` whose key is a unit's key (also a removed one) or its symbol's key, and for `file:` and `unit:` units each entity in their files. | A work item must show the change of a symbol it removed. |
| `dossier check` | Two findings: a feature that names a plan row of the dossier that does not exist, and a plan row whose status starts with `done` or `built` while a feature that names it has an open work item (with the units and tests that are missing). Read at the HEAD of the dossier's workspace, only when the repository keeps code graphs and HEAD has one. | "A plan row with no feature" (A5) is not built: the rows before P3 have no feature, so each would be a finding. |

Not built yet: `planning/README.md` (the reference is in the skill), the feature's start ref and "changed" in
`complete`. A recorded run names its file relative to its crate (`tests/code.rs`); a test key names it from the
repository root, so a run matches a key whose file ends with the run's file; the e2e fixture is one crate at the root,
so only the real repository has the other case.

Tests (`crates/legionctl/tests/code.rs`; ctx and tokei are stand-ins, `tests/fixtures/ctx-standin` and
`tests/fixtures/tokei-standin`, which writes tokei 15's JSON shape):

| Test | It catches |
| --- | --- |
| `a_plan_is_checked_against_the_code_graph_and_a_commit_that_adds_its_units_completes_it` | A check without a graph; each problem of a bad plan at another ref (unknown field, unknown prefix, an `edit` unit and a done item's unit that resolve to nothing, a duplicate id, an unknown story, no tests); each unit `done`, `missing` or `still there` with LOC from tokei (code lines, not all lines) and from the span; the `dossier check` findings; the last run (failed keeps the item open) and the kills of each linked test; a commit that adds the function and the test and removes the old function completes the item; the changes since the plan's commit that touch each item, and none for an item whose file changed elsewhere. |

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

### Built: P5 (2026-10-15)

| Part | Where |
| --- | --- |
| Tables `threads`, `thread_log`, `members`, `messages`, `watch_rules` | `workspace.db` v13 (`crates/legionctl-core/src/thread.rs`) |
| Operations | `thread.start`, `list`, `show`, `join`, `leave`, `send`, `inbox`, `ack`, `declare`, `pace`, `rules.list`, `rules.add`, `rules.rm`, `peers`, `observe` (CLI `thread <name>`, MCP `thread_<name>`); internal `agent.tool` (the `post-tool` hook) and `thread.gate` (the `pre-tool` hook) |
| Delivery | `engine::hooks_run` (prompt, stop), `crates/legionctl/src/hook.rs` (pre-tool, post-tool), `integrations/agent-plugin/hooks/hooks.json` (`PreToolUse` for each tool, new `PostToolUse`) |
| Skill text | sessions-and-agents (rules 19-21, `refs/reference-threads.md`), dossier-practice (`refs/reference-hooks.md`), tool-lifecycle (group `thread`) |

What is different from B2-B8 and B+1, and why:

| Item | Built | Why |
| --- | --- | --- |
| Thread log | Table `thread_log` in `workspace.db`, append-only; each projection changes in the same transaction as its row. The `agent.tool` rows stay in the operator's event log (kind `internal.agent.tool`, the audit of the hook's call). | The operator's event log is in another file and is pruned after its days; the thread log must stay. A tool call of a session that is in no thread is not a thread fact. |
| Watch counts | The columns `run`, `misses`, `done` of `watch_rules`, not a table `watch_counts`. | One rule has one count row. |
| Watch rule | Trigger: a tool of `tools` (`Edit\|Write\|MultiEdit\|NotebookEdit`) on a path that has `path` as a part; expect: a Bash call whose command contains `expect`; at `every` calls the rule sends its message, its count goes back to 0, and `misses` goes up. "Since" is the rule's start time, in UTC. | The operator has no time zone of the user. The path match is text, not from the repository root (a shortcut in the code). |
| Steps done for main | `thread send main0 notice "<text>" --rule <id>` adds 1 to `done` and adds the counts to the text. | The observer that does the step says so; the operator does not run steps in P5. |
| Pace | `on-trigger`, `on-stop`, `on-declare`. A wake is a `thread.wake` row with the count of log rows since the member's cursor, and the cursor moves at once. | No model member runs in P5: the ACP prompt of a wake, the timed paces (`on-event`, `poll`) and the move of the cursor after the prompt come with P6b. |
| Soft-block | Stops one reply end; it does not stop a reply that goes on because a stop hook stopped it (`stop_hook_active`). | Claude Code sets `stop_hook_active`; only a hard-block may stop that reply again. |
| Hard-block | Stops each reply end, also with `stop_hook_active`, and refuses the call of its tool (`--tool`; the same name, or an MCP tool that ends with `__<name>`) until the receiver acks it. A member that leaves makes its blocks `expired`. | B6: a member that is gone cannot hold the main agent. Failed members (P6b) use the same expiry. |
| Rights | `note` and `hint` from any session; `reminder`, `notice`, `result` from a member; blocks from a `thread-observer` member of the thread. | B+1. |
| Receiver | A member name of the thread, or a peer's session id. | B+1 (`thread send --to <peer>`). |
| `SendMessage` | The post-tool hook writes a `thread.message` row (channel `claude-inbox`, receiver, text, `sent` from the tool's `success`) in each thread of the sender. | Decision 1 of B+. The payload is the one Claude Code 2.1.296 sent (recorded in the e2e fixtures). |
| Peers | Sessions from the last hook call, tool call and dossier release of each session in the operator's event log (within `dossier.stale_hours`), the open bindings and the thread members. The dossier of a session is its binding, else the dossier of the workspace that contains its folder; the topic comes from the workspace's branch. State: `running` (prompt, tool call), `idle` (stop, session start), `ended` (session end), `unknown` (no hook call). | One dossier has one binding, so the agents of its topic workspaces are found by the folder of their hook calls. `paused` and `offloaded` (the systemd scope) are not read yet. |
| Channels | `outbox`, and `claude-inbox` for a Claude Code session. | legionctl cannot see if a session started with `--bare`. |
| Observe | `thread observe <peer>`: the peer row and its newest 50 events (the rows of `trace` whose input has its peer id). | `--follow` and `--kinds` are not built: `trace` follows live. |
| Subagents (todo-120) | A peer id is the session id, or `<session>/<agent id>` for a subagent: the `post-tool` hook sends Claude Code's `agent_id` and `agent_type`, so each subagent has its own state, last tool call, workspace (its `cwd`), type (`subagent`), watch rule counts (a member with `--member-session <session>/<agent id>`) and messages. A subagent has no prompt hook: its messages come back as the `additionalContext` of its next `post-tool` hook. Its dossier is its session's binding. | The hooks of a subagent's tool calls carry the lead's `session_id`, so all subagents were one peer with the lead. The recorded fixtures are of a lead; the two fields are Claude Code's documented common hook fields. A subagent stays `running` until the stale time (no `SubagentStop` hook yet). |
| Classes | `start`, `join`, `leave`: workspace; `send`, `ack`, `declare`, `pace`, `rules add/rm`: session; the others read. | B8. |

Not built yet: model members through ACP, wake budgets, timed paces, branch, compensation and the `effects` table
(P6b); archetype files, default watch rules of the `thread-observer` and agent configs (P6a); `thread.commands` (P7).
The prompt hook gives the messages as text on stdout, which Claude Code adds to the context, as the other prompt lines.

Tests (`crates/legionctl/tests/threads.rs`; the real `legionctl hook` with Claude Code's payloads, the CLI and the
real operator; the observer is a member with a session and no model):

| Test | It catches |
| --- | --- |
| `a_watch_rule_reminds_main_once_with_the_count_of_its_misses_and_wakes_only_on_its_trigger` | A lint that does not set the count back; an edit outside the path that counts; the reminder not at the next prompt, or twice; the wrong miss count or start time; a notice that does not count the step done; a wake of an on-trigger member at a stop or a declare; no wake at a firing rule; an on-stop member not woken at a stop, or with the wrong event count. |
| `blocks_hold_main_until_acked_and_only_an_observer_of_the_thread_sends_them` | A block or reminder from a peer outside the thread or from the main agent; a soft-block that stops twice or stops a reply that goes on; a hard-block that does not hold its tool, holds another tool or another session, or lets a reply end before the ack; a reply that is not a note back; blocks of a member that left. |
| `a_send_message_call_of_main_lands_in_the_thread_log` | A `SendMessage` call not in the log, with the wrong member, receiver, text or sent state. |
| `the_subagents_of_one_session_are_peers_with_their_own_workspace_tool_calls_and_messages` | Subagents of one session that are one peer, or one peer with the lead; a wrong workspace, type or last tool call; a message or a watch rule reminder to a subagent that reaches the other subagent, the lead, or nobody, or twice; `observe` of a subagent with the events of another. |
| `the_outbox_survives_a_restart_and_peers_on_one_dossier_see_each_other_with_their_state` | A message lost or given twice after an operator restart; a peer in another topic workspace of the dossier not shown, or a session on another dossier shown; a wrong topic, state (running, idle, ended), last tool, open message count, shared scope or channel; `observe` of a member name or of a session that is not a peer. |

### Built: P6a (2026-10-15)

| Part | Where |
| --- | --- |
| Archetype files | `integrations/agent-plugin/agents/{main-agent,thread-observer,ui-composer}.md`; refs in `integrations/agent-plugin/agent-refs/` |
| Team file and its schema | `integrations/agent-plugin/teams/vanilla.jsonc`, `teams/team.schema.json` (from `archetypes::Team`) |
| Config layer | `agents.configs.<name>` (`config/defaults.jsonc`: one entry per archetype), `crates/legionctl-core/src/archetypes.rs` |
| Operation | `agents.check` (read; CLI `agents check [--team] [--harness claude\|codex]`, MCP `agents_check`) |
| Codex agents | `setup apply` writes `~/.codex/agents/<config>.toml`; `setup check` lists each one |

What is different from B+2 to B+5, and why:

| Item | Built | Why |
| --- | --- | --- |
| Refs | `agent-refs/<kind>-<slug>.md`, named in the text as `${CLAUDE_PLUGIN_ROOT}/agent-refs/...`; for Codex, setup puts the plugin folder in place of the variable. | Claude Code reads `agents/` with its subfolders (plugins/components, "Organize agents in subfolders"), so `agents/refs/` would load as agents. Claude Code puts the plugin folder in place of `${CLAUDE_PLUGIN_ROOT}` in an agent's text. |
| Run paths | `claude-session` and `codex-session` (the main agent), `claude-team` (a member with a Claude Code lead: a teammate, a plugin agent type), `codex-subagent` (a member with a Codex lead). | Decisions 4 and 6 of B+. |
| Fields per path | `claude-session` refuses `sandbox`, `approval`; `codex-session` refuses `tools`; `claude-team` applies the archetype file and, from a config or team, only `model` (the Agent tool's model), `pace`, `budget`; `codex-subagent` refuses `tools`, `mcp`, `account`, `commands`, `sandbox`, `approval`, and from a team also `model`, `effort`, `skills`, `instructions`. `pace` and `budget` are the operator's on each path. | A teammate's frontmatter and text are the shipped file; a Codex subagent inherits sandbox, approval and MCP, and its file is per config. Decision 3: refuse, name the field. |
| Codex install | `setup apply` writes one file per config that is not `"harness": "claude"` (not per archetype, not `agents install --agent codex`), with `name`, `description`, `developer_instructions`, `model`, `model_reasoning_effort`. It refuses all before a write when one config cannot apply; it removes a file that it wrote for a config that is gone, and never replaces a file that it did not write. | A config with its own model must reach Codex; `name` and `description` are the identity of a Codex agent. One setup command for each Codex part. |
| Model | The archetype files set no model. | Claude Code model names do not apply to Codex; a config sets the model of its harness. |
| Budget | One `budget` (`tokens_per_day`, `wake_tokens`, `wake_seconds`) in configs and team members, not `wake_budget`. | One field, one narrowing rule (a higher layer may not raise or remove a limit). |
| Narrowing | Config over archetype and team over config: `tools.allow` and `commands.allow` may not add an item. Repository layer over the user's config: also no removed deny, ask or limit, and no other archetype. `flow.may` may only remove kinds. | B+2, B+3. |
| Team files | The first `<team>.jsonc` of `.legionctl/teams/`, the user's `teams/`, then the plugin's; team files do not merge. `vanilla` has the members and `flow.may`. | `gates`, `stop`, `restart` and `when` come with the flow in P6b. |
| `agents check` | Not run by `make static`. | It needs the operator; the e2e test checks the shipped files. |
| Backfill | The observer's text has the rule (idle only, the dossier lock, append only, `(by observer0)`, a nudge when refused). | Member writes (`--member`, E6) are not built: the operator refuses a write into another dossier, so the observer sends a nudge until then. |

Tests (`crates/legionctl/tests/agents.rs`, the CLI and the real operator):

| Test | It catches |
| --- | --- |
| `vanilla_resolves_its_three_members_through_their_layers_and_a_layer_that_widens_is_refused` | A layer in the wrong order or `set_by` wrong; a team that narrows refused; a team that adds a tool or a message kind, or a repository layer that removes a deny, let through. |
| `a_field_that_a_run_path_cannot_apply_refuses_the_member_and_names_the_field` | `mcp` on a teammate or a Codex subagent let through; a sandbox on a Codex subagent let through; a field refused on a path that applies it (the lead's `mcp`, a Codex lead's sandbox). |
| `setup_apply_writes_each_agent_config_as_a_codex_agent_with_instructions_model_and_effort_only` | Another key in the TOML; a Claude-only config written; the plugin variable left in the text; a user file replaced or a stale file kept; a refused config that writes some files. |
| `the_archetype_files_carry_only_the_frontmatter_fields_that_plugin_agents_honour` | A shipped file with a field that plugin agents ignore or a missing ref; such a field or ref let through. |

### Built: P6b and P6c (2026-10-15)

| Part | Where |
| --- | --- |
| Pools | `crates/legionctl-core/src/pool.rs` (operator.db v12: columns of `threads`, `members`, `watch_rules`; tables `costs`, `effects`); `crates/legionctl-operator/src/thread.rs` (the wake runner) |
| Operations | `thread start --team`, `thread.branch`, `thread.compensate`, `dossier append --member` (the backfill), `thread send --teammate`; internal `thread.teammate` (the hooks `teammate-idle`, `task-completed`) |
| Team file | `vanilla.jsonc`: `flow.gates`, `flow.stop`, `flow.restart`, `flow.rules` (the lint rule of observer0); `team.schema.json` |
| Session env | `LEGIONCTL_MEMBER=<thread>/<member>` and an overlay of main0's folder for a member; `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` for each other Claude Code session that legionctl starts |

What is different from B5, B6, B+2 to B+4 and E6, and why:

| Item | Built | Why |
| --- | --- | --- |
| Run of a member | Chosen at `thread start --team` from the main agent: `claude` gives teammates (team mode, decision 4), `codex` gives Codex subagents (decision 6), any other agent gives operator sessions. A teammate lost at `/resume` (the `SessionStart` hook with source `resume`) runs as an operator session from then on and wakes at once. A member of `thread join` runs itself (`self`), as in P5. | Decisions 4 and 6 name the run for Claude and Codex; the operator session is decision 1's durable path for every other agent and for a lost teammate. The check of the team uses the member's run path (`agents check` resolves teammates and subagents, `thread start` resolves operator members as sessions). |
| Operator member | No session until its first wake: the wake makes it (`<thread dossier>--<member>`, its own topic workspace) and later wakes resume it through `session.acp` (`session/load`). A wake is one ACP prompt: the archetype text, the events after the cursor, the open messages to it and its rules. The cursor moves when the prompt ends; a wake that a restart cut off keeps its ACP session (job `thread.wake`, then `thread.cut-off` in the log) and runs again from the same cursor at the operator's start. | "Sleep more" (B+0): an asleep member costs no process and no worktree; restart as in B6. |
| Triggers | A thread-observer with pace `on-trigger` wakes at a checkpoint of main0, a test change of main0 (a path in `tests/`, `test_*`, `*_test.*`, `*.test.*`), `declare done` or `ready`, and a missed reply rule. A reply rule (`flow.rules`, column `reply`) counts the watched calls and misses only at the stop when the expected command did not come. | B+3 triggers; "a rule finds work" for the process job. Test files by name: the runners of E3 are not built. |
| Budget | `wake_tokens` (the prompt plus the answer, 4 characters a token) and `wake_seconds` end the wake: the operator drops the ACP connection, the cost row has `over`, and the cursor stays. `tokens_per_day` stops the wakes of the day; with the stop `budget`, the member ends. | The gateway's counter counts only the calls it proxies (shortcut in the code). |
| Cost | Table `costs` (not log rows: a wake must not see them as events): `wake` per wake, `inject` per prompt hook that gave thread messages (their tokens and the hook's time). `thread show` sums them per member. | B+4 cost to main. |
| Overlay | The member's sandbox mounts main0's folder (the thread's `workdir`) as an overlay with an empty upper layer in `<runtime>/legionctl/overlay/<session>`, cleared after each wake. | B+3: the process job's steps never change main0's files. |
| Backfill | `dossier append --member <name>`: only an active thread-observer of an open thread on that dossier, only while main0 is `idle` (its last hook call), one line marked `(by <name>)`; the idle check and the write are in one IMMEDIATE transaction of `operator.db`. Else the call is refused, and the observer nudges. Each backfill is an effect, and main0 gets one `notice` with the undo command. | E6 decision 3. There is no file lock on a dossier; the operator's write lock serializes it with main0's hook calls. |
| Effects | `effects`: a backfill (undo: remove that exact line) and each message of a thread-observer (undo: retract). Commits of an observer are not effects. | An observer's patch is a `result` with its commit (B4); `git revert` is main0's choice. |
| Branch | `thread branch <thread> <row>`: the log up to the row (each row keeps `from`), each member that did not leave, active again (operator members get new sessions at their first wake), the rules with their counts, the caller as main0, and the git branch `legionctl/thread-<new>` at the newest checkpoint up to the row. | Workspaces open from that branch with `workspace open --base`. |
| Flow | Gates `checkpoint` (`workspace checkpoint` refuses), `declare:done` (`thread declare done` refuses with the open items), `task:completed` (the `TaskCompleted` hook, exit 2); conditions `<member>: no open hard-block` and `<member>: result <words>`. Stops `main0 ended` (the session-end hook), `declare:done`, `budget`. `restart.max` failed wakes within `restart.within`, then the member fails, its blocks expire and main0 gets a note. | B+4. `main0 ended and idle 30m` needs a timer: built as `main0 ended`. `when: ui` of ui0 is not built: ui0 has no trigger yet. |
| Team mode | The operator gives `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` to each Claude Code session it starts that is no member. `thread start` tells the lead to spawn each member as a teammate of agent type `legionctl:<archetype>`. `TeammateIdle` gives the teammate's pending messages with exit 2 (it goes on with them); `TaskCompleted` holds the task with exit 2 while the gate fails. A teammate shares the lead's session, so it sends with `thread send --teammate <name>`. | Decision 4. The payload fields are the documented ones (`teammate_name`); not seen live. |

Tests (`crates/legionctl/tests/threads.rs`, the CLI, the real operator, the real `legionctl hook` and the stand-in ACP
agent, which runs a script per member):

| Test | It catches |
| --- | --- |
| `a_team_thread_wakes_its_observer_only_on_a_trigger_and_within_its_budget` | A member awake or with a session at the start; a wake for a stop with nothing missed, or for main0's own lint; no wake for a missed lint or a test change; a lint that changes main0's folder; no notice, wrong counts, or a notice twice; the teams env var on a member; a wake past its budget with no `over`, or with the cursor moved; no cost of main0. |
| `the_observer_backfills_only_while_main_is_idle_and_the_flow_gates_done` | A backfill while main0 runs; no nudge; a missing mark; the notice twice or never; a compensation that leaves the line or runs twice; `declare done` before the verification; no end of the pool at done; a branch with rows after the point, without its members or rules, or with its git branch at another commit; a parent changed by its branch. |
| `an_operator_restart_restores_the_members_and_a_cut_off_wake_goes_on` | A cut-off wake not run again, run in a new ACP session, or from another cursor; the notice twice. |
| `a_claude_lead_runs_the_team_as_teammates_and_a_lost_one_restarts_as_an_operator_member` | No teams env var for a Claude lead; members started as operator sessions with a Claude main; a teammate's message lost or given twice; a task or a checkpoint that passes an open hard-block of the teammate; a lost teammate not run as an operator session. |
| `a_codex_lead_refuses_a_member_config_that_sets_its_own_sandbox` | A Codex subagent config with its own sandbox let through. |
| `a_failed_member_restarts_within_its_limit_then_its_blocks_expire` | A restart loop past `max`; a dead observer that still blocks the main agent; no note to main0. |

Not checked live: Claude Code's team mode (a lead spawning the teammates, the `TeammateIdle` and `TaskCompleted`
payloads and their session id, a `SessionStart` with source `resume` without the teammates), and the real ACP
adapters as members. The operator side of each is tested with the stand-ins.

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

### Built: P7 (2026-10-15)

| Part | Where |
| --- | --- |
| Thread screen | `legionctl tui [--thread <id>] [--agent <name>] [--dossier <name>]` (`crates/legionctl/src/tui.rs`, ratatui) |
| Agent commands | The operator records each ACP `available_commands_update` as the event `session.commands` (`acp.rs`); operation `session.commands <session>` (read) gives the newest list |
| Chat with an agent | `session.run` takes `acp` (go on in the ACP session of an earlier run) and gives `acp` back |
| Keys and commands | `integrations/agent-plugin/skills/tool-lifecycle/refs/reference-thread-screen.md` |

What is different from C2 and C3, and why:

| Item | Built | Why |
| --- | --- | --- |
| The chat bar's agent | `--agent <name>`: each prompt is a headless run (`session run`) that goes on in the ACP session of the run before it (`session/load`); a run whose agent cannot load sessions fails. Not `#main0`. | P6b is not built: no member has a live ACP session. When it comes, the chat bar sends to main0. |
| Agent commands | Kept per session (event `session.commands`, operation `session.commands`), not on the member with `thread.commands`. The list comes with the first run, so `/agent:<name>` is refused until the agent ran once. The file scan of the command folders (no ACP) is not built. | No member has an ACP session yet (P6b). A list that the agent did not give is a guess. |
| Fold keys | `Alt+1` and `Alt+2`, and `Ctrl+1`, `Ctrl+2` where the terminal reports them; `/fold 1`, `/fold 2`. | A plain xterm sends no key for `Ctrl+1`. |
| Chat bar | 60% of pane 3 at the start, `Ctrl+Right` and `Ctrl+Left` in steps of 10% (30% to 100%), taller with its text (`Alt+Enter` or `Ctrl+J` adds a line) up to 40% of the height, then the newest rows. | A terminal has no drag. |
| Pane 2 | The thread's log (the newest open thread, or `--thread`) and the runs of this screen; it reads the thread each second. No filter by member. | The event stream (`GET /v1/events`) is for the app (P8); the TUI reads as the dashboard did. |
| Views | Pane 3 shows one of the dashboard, resources, sensors and the output of the last command (`d`, `r`, `s`, `o`, `/view <name>`); pane 1 lists them. A view that cannot read shows the error in its frame. | JSONC views, `views.*` and the `ui-composer` are P8. |
| Commands | `/` gives the agent's commands (marked `(agent)`), the screen's own (`/fold`, `/view`), then every operation of the registry with the CLI's words and its class; for an agent caller a workspace operation says that it asks the user. The operator gates each call as from the CLI. | One screen for a person and an agent (C1). |

Tests (`crates/legionctl/tests/tui.rs`; a pty with a small terminal emulator, so the test reads the place of each
cell; the real operator, a real thread and the stand-in ACP agent):

| Test | It catches |
| --- | --- |
| `the_thread_tui_sizes_its_panes_folds_them_and_grows_the_chat_bar` | Pane widths other than 7%, 30% and the rest; a fold that does not give pane 3 the width or has no folded column; a thread's log not in pane 2, or not read again; a chat bar not at 60% centred, not taller with three lines, or not wider after `Ctrl+Right`. |
| `the_thread_tui_runs_an_agent_command_and_a_registry_operation` | The agent's commands not kept or not offered, or offered after the registry; `/agent:plan x` not sent as `/plan x`, or in a new ACP session; a registry command not run with the CLI's words; runs not in pane 2; `session commands` of a session that does not exist answered. |

### Built: P8 (2026-10-15)

| Part | Where |
| --- | --- |
| App thread screen | `legionctl-app [<page>] [--agent <name> [--dossier <name>]]` (`crates/legionctl-app/src/thread.rs`) |
| Chat bar, shared | `legionctl_api::chat` (the `/` entries, what a line does, the log rows of pane 2) and `legionctl_api::cli` (the CLI's words), for the TUI and the app |
| Views | Format, checks, files: `crates/legionctl-core/src/views.rs`; schema `docs/api/view.schema.json`; operations `views.list`, `views.check`, `views.save`, `views.render`, `views.place` (`crates/legionctl-operator/src/views.rs`) |
| Import | `thread import <thread> <account>` (`thread.import`) |
| Keys and fields | `integrations/agent-plugin/skills/tool-lifecycle/refs/reference-thread-screen.md` |

What is different from C2 to C5, and why:

| Item | Built | Why |
| --- | --- | --- |
| App page | The whole window is the thread screen: pane 1 holds the old pages (Now, Apps & GPU, Health, Bench, Resources) as icons, and the thread's view. Not a page "Threads" next to them. | The same screen as the TUI (C1); the old tab bar and pane 1 did the same job. |
| Pane 2 | The newest open thread, read again at each event of `GET /v1/events`. The stream runs on the app's tokio runtime; the screen takes its events each 100 ms. | GPUI's test scheduler refuses a wake from a tokio thread. A timer that takes events is not a poll of the operator. |
| Chat bar | Click to type; `Shift+Enter` adds a line; `Ctrl+Left` and `Ctrl+Right` (30% to 100%). No drag. | The keys are the TUI's; a drag handle was not asked for in P7 and is not built. |
| The chat bar's agent | As in P7 (`--agent`, a headless run), on the thread's account. | P6b is not built. |
| Layout | `views place` writes the row `views.place` (view, parameters, repository) into the thread's log; the screens show the newest one. No `layout` column. | The log is the source (B2); an import copies it. |
| View operations | `views.show` is not built: `views list` gives each file, layer and widget, and `views render` gives the data. | One operation less with the same facts. |
| View class | `views.save` is class workspace (it writes the user's or the repository's files); `views.place` is class session; the others read. A view's source runs as the caller, with the same input checks as a call from outside. | A view cannot run a change, and a session's view cannot read as another session. |
| Thread account | No column: the account is the newest `thread.import` row's, else the main session's account. The original's state becomes `imported` (read-only: join, send, rules, pace and place are refused). | The thread tables were in move (P9 put them in `operator.db`, with `sessions`); rows in the log need no schema step, and an import copies them. |
| Import | Copies the log, the members (without their sessions, which run on the old account; their cursor is the import row), the messages, the rules and main0's Claude transcript (the code of `session resume --account`). A second import of a thread is refused: the copy exists. | C5. A member session on the new account starts with P6b. |
| ui-composer | The format, the files, the checks and `views save`/`views place` are there for the model and for a person; the model's wake is P6b. | P6b is not built. |

Tests:

| Test | It catches |
| --- | --- |
| `app.rs::the_app_thread_screen_sizes_its_panes_folds_them_and_grows_the_chat_bar` | Pane widths other than 7%, 30% and the rest at 1000 px; a fold that is not a 24 px rail or does not give pane 3 the width; a rail that does not open; a chat bar not at 60% centred, not taller with three lines, taller than 40%, or not wider after `Ctrl+Right`. |
| `app.rs::the_app_chat_bar_offers_the_agents_commands_first_and_runs_a_registry_line` | The agent's commands not offered first or without the hint; `/agent:plan x` not sent as `/plan x` or not in the same ACP session; a registry line not run with the CLI's words; runs not listed. |
| `app.rs::the_app_thread_page_updates_from_the_event_stream` | A placed view not shown with its table; a new member or a changed view file not shown without a poll. |
| `threads.rs::a_view_cannot_bind_a_change_operation_or_a_widget_outside_the_set` | A widget outside the set or a change operation accepted or written; a view file not kept as written; `views render` rows other than the members; a missing parameter not refused; the repository layer not first. |
| `threads.rs::a_thread_imported_to_another_account_copies_its_log_and_members_and_imports_once` | Log rows, members, messages, rules or the transcript not copied; the copy without its account or origin; a second import, a send to the original, an import to the same account or to a missing account accepted. |

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

## Decisions on Part B+ (user, 2026-10-15)

1. Messages: the thread outbox AND Claude Code's cross-session `SendMessage`. legionctl uses `SendMessage` directly
   for Claude peers (fast, Claude only, not durable), and the outbox for every member (durable). The `post-tool` hook
   records each `SendMessage` in the thread log, so the log stays complete.
2. Config place: layered. The archetype file is the lowest layer; `agents.configs.<name>` is above it; the
   repository layer and the team flow may only narrow.
3. A config field that a run path cannot apply: the start is refused and names the field.
4. Team mode: when the main agent is Claude Code, `vanilla` runs as a Claude Code agent team by default (visible in
   Claude's agent panel); other harnesses run members as operator sessions. The operator sets
   `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` for the sessions it starts. Limits that follow (research, dossier
   MEMORY/12): one team per session, no nesting, a fixed lead, in-process teammates not restored after `/resume`
   (the operator starts them again from the thread's members), spawning needs an interactive session.
5. Dossier writes (replaced by Part E, E0 decision 3: a marked backfill while the main agent is idle): no exception. An observer writes only its own dossier; for a missing record in the main agent's
   dossier it sends a nudge.
6. Codex members run as Codex subagents of the main Codex session (`~/.codex/agents`), with the inherited sandbox,
   approval and MCP. With decision 3, a Codex member config that sets its own sandbox, approval or MCP is refused.

With these, the process job in B+ changes: the observer still runs the steps it can do in its overlay (lint,
targeted tests) and makes no checkpoint and no dossier write for the main agent; it sends a `notice` with the
result and a nudge for the record or the checkpoint.

## Part E: project checks, test runners, graph schema, change watcher, dossier home, observer recovery (todo-100..todo-104)

Status: approved (2026-10-15), an addition to the design; P9 built. The user's words, the breakdown and the four
user decisions are in the dossier: `MEMORY/13-project-checks-watcher-dossier-home.md`. The decisions of Parts A, B
and B+ stay binding, except B+ decision 5, which E0 decision 3 replaces. Part B and B+ internals (threads, outbox,
watch rules, presence; P5, built now) are used only through the behaviour that B2-B6 and B+1-B+4 state.

### E0. User decisions (binding, 2026-10-15)

1. Dossier home: each dossier moves to `~/agentWorkspaces/<descriptive-name>/`. A stable id in `dossier.jsonc`
   names it; a rename changes only the folder. The operator migrates `~/.agentworkspace` once and removes it. All
   SQLite of a dossier lives in its home.
   Addition (same day): each directory where a dossier is used gets the link `<root>/agentWorkspaces` →
   `$HOME/agentWorkspaces` (the whole home; the bound dossier is `agentWorkspaces/<name>`). It replaces the
   `.agentworkspace` link everywhere. A real folder `<root>/agentWorkspaces/` is imported first (E1a).
2. Code-graph snapshots stay in the repository (`.graf/refs`, as built).
3. The observer may backfill missing records into the main agent's dossier: each marked `by observer0`, through the
   dossier lock, only while the main agent is idle, never an edit of the main agent's own text, and it tells the
   main agent what it backfilled. This replaces B+ decision 5.
4. The root daemon (`legionctl-system`) runs the file-system watcher on the dossier homes and the attached
   repositories. It never writes a user's file as root, it watches only registered paths, and it survives restarts.

### E1. Dossier home and the SQLite split (todo-103, first part)

What and why: one folder per dossier holds all of its data, so a dossier is one unit to copy, back up and read. A
stable id lets a person rename the folder to a descriptive name.

```text
~/agentWorkspaces/
  <descriptive-name>/
    dossier.jsonc          "id": "dsr-<ulid>" (new, never changes), title, entities, objectives
    plan.md  MEMORY/  DESIGN/  EVIDENCE/  SPIKE/
    queries/<name>.jsonc   queries of this dossier (E4)
    .state/dossier.db      SQLite (WAL): this dossier's rows (table below)
    .state/runs/<run>/     logs and profiles of check and test runs (E2)
```

| Data | Today (`~/.agentworkspace/workspace.db`) | After |
| --- | --- | --- |
| `status_changes`, `nudges`, `writes` | one file for all dossiers | `<home>/.state/dossier.db` |
| `annotations`, `test_runs`, `mutants`, `test_accepts` | one file, per repository | `dossier.db` of the session's dossier; a read takes the union of the dossiers that attach the repository (E1 rule 4) |
| P5's thread tables (B2) | one file | `dossier.db` of the thread's dossier (`threads.dossier`) |
| new: `check_runs`, `changes`, `change_entities`, `diagnostics`, `backfills` | — | `dossier.db` |
| `bindings`, `sessions`, `workspaces` (rows that span dossiers) | one file | `~/.local/state/legionctl/operator.db`, by dossier id |

Rules:

1. A dossier is found by id. The operator keeps `id → folder` in `operator.db` and checks it at each start and at
   each watcher event on `~/agentWorkspaces` (a moved folder keeps its id; two folders with one id: the second is
   refused and named in `doctor`).
2. Bindings, events, links (`<dossier>#<row>` in `planning/`) and workspace branches use the name for people and
   the id for the record. `planning/` links keep the name form; `code plan check` resolves a name through the
   registry, also an old name (the registry keeps renames).
3. `dossier.root` becomes `~/agentWorkspaces`. Paths outside it are refused as before.
4. Rows that count for a repository (kills, runs, annotations) are written into the writer's dossier and read as the
   union of `dossier.db` of each dossier whose `entities` name the repository (SQLite `ATTACH`, at most 10 at once;
   more: one read per file, merged).

Migration (one time, an operator job `dossier.migrate`; built as `legionctl dossier migrate`, only when the user runs it, see "Built: P9"):

1. Wait until no binding is live (a live one: the job waits and `doctor` says why).
2. Back up `workspace.db` to `~/.local/state/legionctl/backup/workspace.db.<time>` (online backup).
3. For each `~/.agentworkspace/<name>/`: write an `id` into `dossier.jsonc` when it has none; move the folder to
   `~/agentWorkspaces/<name>/` (`rename` on one file system, else copy, compare the file hashes, then remove).
4. Split the rows by dossier into each `dossier.db`, and the shared rows into `operator.db`. A repository row with
   no dossier goes by its session's binding (the `dossier.use` events), else to the oldest dossier that attaches the
   repository.
5. In each main checkout and open workspace (the `workspaces` rows have the paths): remove the `.agentworkspace`
   link and its ignore line (`.gitignore` staged, not committed), then do E1a there.
6. Compare the row counts per table, then remove `~/.agentworkspace`. A failure at any step stops the job before
   step 6; a new start does the steps again (each step is idempotent).

### E1a. The directory link and the import (decision 1, addition)

At the first dossier use in a directory (`dossier use`, `workspace open`, `session start`), the operator does
these steps in `<root>` (the repository root, else the folder):

1. `<root>/agentWorkspaces` is a link to `$HOME/agentWorkspaces`: nothing to do. A link to another place is refused
   with its target (`doctor` names it); legionctl does not replace a link that the user made.
2. It is a real folder: import it (rules below), then replace it by the link.
3. It does not exist: make the link.
4. In a git repository: when `git check-ignore -q agentWorkspaces` fails, add `/agentWorkspaces` to
   `<root>/.gitignore` and stage the file (`git add .gitignore`; no commit). The pattern has no trailing `/`: git
   sees a link as a file, and `agentWorkspaces/` matches only a folder. Then `git check-ignore` must pass, else the
   step fails with git's answer. A plain folder (no git) skips this step.
5. Worktrees: each worktree (main checkout and each workspace) gets its own link, because a session works there.
   The staged line is only in the main checkout's index; until it is committed and on the workspace's branch, step 4
   in a worktree adds `/agentWorkspaces` to `<common dir>/info/exclude` (one file for all worktrees, no commit, as
   for `/.graf/`), and `git check-ignore` passes.

Import of a real `<root>/agentWorkspaces/` (data is never lost):

1. Each first-level subfolder is one old dossier; files at the first level and deeper folders are not dossiers
   (files are listed in the result and stay in the folder until the user moves them; the folder is then not
   replaced, and `doctor` names it).
2. Copy each subfolder to a temporary folder in `$HOME/agentWorkspaces/.import-<time>/<name>/`, compare the file
   hashes, write an `id` into its `dossier.jsonc` (make the file when it is missing), register it, then `rename` it
   to `$HOME/agentWorkspaces/<name>/`.
3. Name clash: a name that exists in the home gets the next free `<name>-imported-<n>`; the result names each
   renamed dossier. The source folder names cannot clash with each other (one folder level), so only the home can
   clash.
4. Only after each subfolder is registered and in place, the source folder is moved to
   `~/.local/state/legionctl/backup/agentWorkspaces-<root hash>-<time>/` (not deleted) and the link is made.
5. A failure at any step leaves the source folder as it is; the copies of that run stay in `.import-<time>/` and the
   next start finishes them (a registered copy is not copied again). The import is a job (`dossier.import`) with
   one event per dossier.

### Built: P9 (2026-10-15)

| Part | Where |
| --- | --- |
| Home, ids, registry, rename | `dossier.root` `~/agentWorkspaces`; `dsr-<ulid>` in `dossier.jsonc` (a new dossier, a folder without one, each import); tables `dossiers` and `dossier_names` in `operator.db`; `dossier rename <name> <to>` (class workspace) and `sync_registry` at each operator start and in `doctor` (`crates/legionctl-core/src/dossier.rs`) |
| SQLite split | `operator.db` v11: `bindings`, `workspaces`, `sessions`, the thread tables, the registry; `<home>/.state/dossier.db` v1: `writes`, `status_changes`, `nudges`, `annotations`, `test_runs`, `mutants`, `test_accepts` |
| Repository facts | Written into the dossier of the workspace that holds the folder, else the oldest dossier attached to the repository; read as the union of the attached dossiers, one `ATTACH` at a time into one database in memory (no limit of 10) |
| Migration | Operation and job `dossier.migrate` (`legionctl dossier migrate [--dry-run]`, class workspace; `crates/legionctl-core/src/migrate.rs`), only when the user runs it (user decision 2026-10-15: never at an operator start). Until it is done, each other dossier operation, also `dossier list`, refuses with "the dossiers are not migrated yet: run legionctl dossier migrate"; `doctor` says the same or names the failed run. `--dry-run` lists the live bindings that refuse it, the backup, each move with its id or clash, the counts it compares (from a copy of `workspace.db` at v14 in the temporary folder) and the places it links, and changes nothing. A run that an operator restart cut off ends as failed with "run legionctl dossier migrate again" |
| Link and import | `crates/legionctl-core/src/home.rs`: `dossier use`, the session-start rebind, `session start`, `workspace open` (each worktree, and a plain folder's source) |
| Text | 18-workspaces, 12, 13, 14, README, skills dossier-practice (rules 1, 21-23), workspace-practice (rule 4), the threads reference, the plugin command `/legionctl:dossier` |

What is different from E1, E1a and E7, and why:

| Item | Built | Why |
| --- | --- | --- |
| Thread tables | In `operator.db`, not in the thread's `dossier.db`. | A thread's members come from several dossiers (B4: each observer has its own dossier), so its rows span dossiers: decision 1 puts those in `operator.db`. Thread, message and rule ids stay unique without a second id scheme. |
| Rows that name a dossier | `bindings`, `workspaces`, `sessions` and `threads` keep the name; a rename (or a folder renamed by hand, found by its id) changes them in one transaction and records the old name. | The same result as rows keyed by id, without a lookup in each query; old names still resolve (`planning/` plan rows in `dossier check`). |
| Attached dossiers | A dossier is attached to a repository when it has a binding or a workspace there, or its `entities` name it. | No dossier has `entities` yet; the bindings and workspaces have the paths. |
| Writer | The dossier of the open workspace that holds the folder, else the oldest attached dossier; none: refused. | `code tests run`, `mutant record` and the pre-commit gate have no session; the folder names the workspace. |
| Live binding | Not waited for and not refused: it moves as a row into `operator.db`, and its session goes on with its dossier after the move. The dry run lists each live binding. | Until the migration ran, each dossier operation refuses, so nothing writes a dossier during it; the session that runs the migration is itself bound (a refusal would be a deadlock: `dossier release` refuses too). |
| Hooks before the migration | `session-start` and `prompt` answer with the one line "the dossiers are not migrated yet: run legionctl dossier migrate"; `stop` and `pre-tool` do not block or deny (the operations they call refuse, and a hook error never stops the agent). | A live session must not stop because of the old layout. |
| Step 2 | The first run backs up `workspace.db` as it was (`VACUUM INTO`); a run after a cut keeps that backup. | The file changes to v14 after the backup. |
| Step 3 | The registry gets the id and the old folder before the move; a copy across file systems that a cut left (same id) is made again. | A cut after the move still finds the dossier's rows by its origin. |
| Step 6 | What is left of `~/.agentworkspace` (old backups, the empty folder) moves to `~/.local/state/legionctl/backup/agentworkspace-<time>`, not deleted. | Data is never lost; the old `.bak` files of the user stay. |
| Rows without a dossier | Rows of a dossier whose folder is gone, and nudges of sessions without a binding, are counted as dropped in the job result; the count compare includes them. | They have no home; the backup keeps them. |
| Import | Synchronous in the call (one `dossier.import` job row and one event per dossier). The copies go to `.import-<source hash>/`; a cut-off import is finished by the next use there, which copies again each dossier that is not in place (the source stays until the end). | The first use needs the link. One staging folder per source, so no copy of a cut-off run stays behind. |
| Write guard | A path through an `agentWorkspaces` link counts as the path it leads to; `.state` is read-only in a session sandbox. | The link shows every dossier: without this, a session could write another dossier or its own test evidence. |

Tests (`crates/legionctl/tests/e2e.rs`, the CLI and the real operator, temporary homes; the old home from
`tests/fixtures/workspace-v13.sql`):

| Test | It catches |
| --- | --- |
| `the_dossiers_move_to_their_homes_once_and_keep_their_ids_links_and_rows` | A migration at an operator start; a dossier operation (also `dossier list`) that does not refuse with the one message; a dry run that changes a file or does not list the live binding, the backup, a move, the clash, a count or a link; an id that a dossier has before the move and does not keep; a cut at the backup, the move, the split or the links that loses or doubles rows on the next start; an id that changes; a row in the wrong `dossier.db` (session binding before repository); kills not keyed; a count compare that does not hold; the old link or line left; the line committed; a worktree not ignored; a second migration. |
| `the_first_use_in_a_repository_imports_its_agentworkspaces_folder_and_links_the_home` | A first-level file taken as a dossier or the folder replaced while it is there; a name in use overwritten (must be `-imported-1`); a cut-off import that copies again; the source not in the backup; `agentWorkspaces/` with a slash, committed, or not staged; a worktree where `git check-ignore` fails; a plain folder with a `.gitignore`; a user's link replaced; the home imported into itself. |
| `facts_written_from_one_dossier_are_read_from_another_and_a_rename_keeps_rows_and_links` | A kill in the reader's dossier or not read from the writer's; a write from a workspace that goes into the oldest dossier, not the workspace's; a rename to a used name; a rename that loses the id, the binding, the workspace row, the facts or the link; the next fact in the old dossier; a write into another dossier through the link. |
| `the_hooks_of_a_live_session_pass_before_the_migration_and_gate_after_it` | A hook that blocks a prompt, blocks a stop or denies a tool because the dossiers are not migrated, or that does not say to migrate; a migration that refuses a live binding; a binding that does not survive the move; a write guard or prompt gate that does not hold again after the move. |
| `a_session_runs_the_agent_in_a_sandbox_where_the_git_gate_and_read_only_home_hold` (changed) | A session sandbox that can write its dossier's `.state/dossier.db` (the test gate's evidence). |

Not built in P9: the watcher events on `~/agentWorkspaces` (P11; the registry reads the folders at each operator
start and in `doctor`), `code plan check` against the registry (it checks only the `<dossier>#<row>` form).

### E2. Project checks (todo-100)

What and why: each repository declares how it is linted, formatted, checked, built, deployed and installed. Agents,
the test gate and the observer then run the repository's own commands, not a guess or a fixed `make lint`.

Where: the config key `checks` in the repository layer (`<repo>/.legionctl/checks.jsonc`, versioned with the code),
so a branch can change its checks and `config explain checks.lint` names the file.

```jsonc
// <repo>/.legionctl/checks.jsonc
{ "checks": {
  "format":  { "kind": "format", "run": "cargo fmt --all --check", "fix": "cargo fmt --all" },
  "lint":    { "kind": "lint",   "run": "make lint", "gate": "checkpoint" },
  "static":  { "kind": "static", "run": "cargo clippy --workspace --all-targets -- -D warnings" },
  "build":   { "kind": "build",  "run": "cargo build --profile test-fast --workspace --bins --locked", "gate": "merge",
               "profiles": ["target/cargo-timings/cargo-timing.html"] },
  "e2e":     { "kind": "test",   "run": "cargo test --profile test-fast --workspace --no-fail-fast -j8", "gate": "merge",
               "runner": { "format": "libtest",
                           "list": "cargo test --profile test-fast --workspace -- --list --format terse",
                           "one": "cargo test --profile test-fast -p {crate} --test {file} -- --exact {fn}" } },
  "e2e-root": { "kind": "test",  "run": "cargo test --profile test-fast --workspace -- --ignored",
               "runner": { "format": "libtest", "requires": ["root", "kvm"] } },
  "package": { "kind": "deploy", "run": "make package" },
  "install": { "kind": "install", "run": "make install-package", "ask": true }
} }
```

| Field | Meaning |
| --- | --- |
| `kind` | `format`, `lint`, `static`, `types`, `build`, `test`, `deploy`, `install` (closed set) |
| `run`, `fix` | the command (in the workspace, as the user); `fix` changes files, so only the main agent runs it |
| `gate` | `checkpoint` or `merge`: E2 rule 4 |
| `profiles` | files the run writes that the run folder keeps |
| `runner` | for `test` only: E3 |
| `requires` | probes from a closed set: `root`, `kvm`, `docker`, `gpu`, `tool:<name>`, `env:<VAR>` |
| `ask` | the run needs the user's approval (`install` and `deploy` by default) |

Rules:

1. `checks.required` (default `["format", "lint", "build", "test"]`) names the kinds each repository must have.
2. A missing kind is a finding in `doctor` (group Agents) and in `dossier check` for each dossier that attaches the
   repository. At `session-start` the agent gets one line (config `checks.nudge`, as `workspace.nudge`): "This
   repository has no checks of kind lint, test: run `code checks detect` and write `.legionctl/checks.jsonc`."
3. `code checks detect` reads the repository (Makefile targets, `Cargo.toml`, `package.json` scripts,
   `.pre-commit-config.yaml`, `pyproject.toml`) and prints a proposed `checks.jsonc`. It writes nothing; the agent
   reviews and writes the file. A check whose command is not found (exit 127) is a finding "check broken".
4. Gates: `workspace checkpoint` needs a passing run of each `gate: checkpoint` check at the tree that it commits
   (the working tree's hash, E5); without one it runs them first. `workspace merge` needs the same for `gate: merge`
   at the rebased head. A failure refuses with the check, the exit code and the log path. This uses the A7 gate
   path; the fixed command of P4's `code tests run` is removed: it runs the repository's `test` checks.
5. Each run is a job (`code.checks.run`) in a transient user unit (`systemd-run --user --wait --collect`), so its
   profile comes from the cgroup: wall time, CPU time, memory peak, IO bytes. It goes in `check_runs` (repository,
   tree, check, kind, exit, wall, cpu, memory peak, io, job, session, member, time) and in `.state/runs/<run>/`
   (`log.txt`, `profile.json`, the `profiles` files). `checks.keep_runs` (20 per check) limits the folders; a run
   that an EVIDENCE front matter names (`runs: [<run>]`) stays.
6. The observer (B+3 process job) runs the missed `lint`, `format`, `static`, `types` and `test` checks in its
   overlay, never `fix`, `deploy` or `install`. The verification job runs each `gate: merge` check at `declare done`.

API (group `code`): `code checks list` (each check, its layer, its last run per tree; read), `code checks detect`
(read), `code checks run [<check>...] [--kind] [--changed]` (session; `--changed`: only checks whose `paths`, when
set, match the change), `code checks show <run>` (read: the log tail and the profile).

### Built: P10 (2026-10-15)

| Part | Where |
| --- | --- |
| Checks config | Config key `checks` (`crates/legionctl-core/src/checks.rs`, `ChecksConfig`): `required`, `nudge`, `keep_runs` and each other key a check by name; this repository's own file is `.legionctl/checks.jsonc` (format, lint, static, types, build, build-static, e2e, package, install) |
| Language files (todo-113) | `config/languages/{rust,node,python}.jsonc`, compiled in: manifests, build outputs, the checks that `detect` proposes, the tools (toolchain, LSP, formatter, linter, analyzers, profiler, debugger). Rust: rustup, rust-analyzer, rustfmt, clippy; Node: bun, Biome, tsc; Python: uv, ruff, ty. A new language is a new file and one line in `LANGUAGES` |
| API | `code checks detect` (read), `code checks list` (read), `code checks run [<check>...]` (session), `code checks show <run>` (read) |
| Runs | `run_check`: `sh -c <run>` in the top folder of the workspace; log, `profile.json` and the `profiles` files in `<writer dossier>/.state/runs/<job>-<check>/`; row in `check_runs` (`dossier.db` v2, the step from v1; a repository fact, read as the union of the attached dossiers) |
| Gates | `workspace checkpoint` (after the test gate, before the commit) and `workspace merge` (after the rebase, before the fast-forward) call `Dossiers::gate`; a run counts when its repository, tree, check name and command match and its exit code is 0 |
| Findings and nudge | `doctor` (each open workspace), `dossier check` (the dossier's workspace) and the session-start lines (the workspace's branch, else the main checkout) |
| `code tests run` | Runs each `test` check; with `runner.format: libtest` it records each test as before. The fixed command of P4 is removed |

What is different from E2, and why:

| Item | Built | Why |
| --- | --- | --- |
| Profile | From `wait4` (the kernel's `rusage` of the command and each process it waited for): wall and CPU time, the largest resident set of one process, storage bytes read and written. No transient user unit. | The test operator and CI have no user systemd manager, and a second path for them would be a fallback. The memory value is the peak of the largest process, not of the cgroup. |
| Tree | The git tree of the working tree (tracked and untracked files, not ignored ones), from an index file of its own (`tree()`), until E5 gives its hash. | It is the tree that the checkpoint commits and, after the rebase, the merged head; the repository's index does not change. |
| Gate order | A gate runs its checks in name order and stops at the first failure. | One order that a person can see in `code checks list`; this repository names `build-static` so it runs before `e2e`. |
| `deploy`, `install` | `code checks run` refuses them: the user runs the command. No `ask` field. | The approval flow for a check is not built; refusing is safe. |
| Fields | `requires`, `ask`, `paths`, `runner.list`, `runner.one` and `code checks run --kind`, `--changed` are not built; a file with them is refused (unknown field). | They belong to E3 (P12) and E5 (P11). |
| Folder workspaces | No gate (they have no tree). | A folder workspace has no git commit. |
| Detect | Reads the language files only (their manifests in the top folder); Makefile targets, `package.json` scripts and `.pre-commit-config.yaml` are not read. With two languages each name gets the language first (`rust-lint`). | The user decision makes the languages data (todo-113); the agent reviews and edits the proposal. |
| Tools on change | Nothing runs on a file change. | User decision: opt-in per repository and tool, with the watcher (P11). |

Tests (`crates/legionctl/tests/checks.rs`, the CLI and the real operator on fixture repositories; changed in `e2e.rs`):

| Test | It catches |
| --- | --- |
| `detect_proposes_the_checks_of_each_language_from_the_language_files` | A language file that does not parse or proposes other commands or tools; two languages with names that collide; a repository with no known manifest that is not refused; a proposal that is not a valid checks file; `detect` that writes a file. |
| `a_checkpoint_runs_its_gate_once_per_tree_and_a_failing_check_refuses_with_its_output` | A checkpoint that commits past a failing check, or refuses without the exit code, the log path or the output; a passing run at the tree that runs again; a run at another tree that counts; a run without its log, profile or `profiles` files; `keep_runs` that keeps too many folders or removes one that an EVIDENCE names. |
| `a_merge_refuses_without_passing_build_and_test_runs_at_the_rebased_head` | A merge that counts a run at the branch before the rebase; a merge past a failing check; a gate that goes on after a failure; a merge that does not run the checks at the new tree. |
| `a_missing_check_kind_is_a_finding_and_a_session_start_nudge` | A missing kind not in `doctor`, `dossier check` or the session start; exit 127 not a "check broken" finding; a nudge or finding from the main checkout when the workspace's branch has the kinds. |
| `a_test_run_records_each_test_with_its_file_result_and_ref`, `a_test_run_that_does_not_build_fails_with_the_compiler_error` (changed) | `code tests run` that does not run the repository's test check, puts the filter in without `--`, is not a check run, or runs without a test check. |

Not built in P10: the observer's runs (B+3, rule 6), the verification job's runs at `declare done`, the tooling
manifest with install sources and hashes (todo-114) and build-output size tracking (todo-111).

### E3. Test runners in the graph (todo-101)

What and why: a test is useful evidence only when it is planned, present and runnable here. The three sets differ,
and the difference is the work that is left.

| Set | Source | A test is in it when |
| --- | --- | --- |
| planned | `planning/` (story `tests`) | a story names its key |
| actual | the tree (P4's scan of test functions; the code graph's function node) | the function exists at the tree |
| runnable | a `test` check's runner (E2) | the runner's `list` command gives it here and each `requires` probe passes |

- The runner lists with its own command (`cargo test -- --list --format terse`, also with `--ignored`), so an
  `#[ignore]` test is runnable only through a runner whose `requires` passes (for example `root`, `kvm` in CI).
- `code tests run <test>` runs one test with the runner's `one` template; `code tests run --changed` runs the
  tests of the work items that the open changes touch (E5).
- Each test gets one state: `planned only` (not in the tree), `not runnable` (with the reason: no runner lists it,
  or the failed probe), `runnable`, then its last run (`passed`, `failed`, `ignored`) with tree and age.
- A container runner sets `"in": "container"`; it uses the `workspace pack` image (W5) and the same `one`
  template.

Stories with variations. A story may declare the branches of its behaviour and the expected result of each; a
test entry names the variations that it covers:

```jsonc
{ "id": "S2", "kind": "story", "text": "A checkpoint runs the checkpoint gates.",
  "variations": [
    { "id": "V1", "given": "a passing lint run at this tree", "expect": "success", "detail": "no new run" },
    { "id": "V2", "given": "no run at this tree", "expect": "success", "detail": "the gate runs lint first" },
    { "id": "V3", "given": "lint fails", "expect": "failure", "detail": "refused with the check and the log path" }
  ],
  "tests": [ { "test": "test:crates/legionctl/tests/code.rs::a_checkpoint_runs_its_gates", "covers": ["V1", "V2", "V3"] } ] }
```

- `expect` is `success` or `failure`; `detail` is the exact outcome (an output, a state, an error text).
- A test entry is a key (the story has no variations) or `{test, covers}`. `code plan check` refuses an unknown
  variation id, and for a `done` item a variation that no test covers.
- The thread-observer's test review (B+3) reads each `failure` variation's test and sends a `hint` when the test
  does not assert the failure (`detail`). This is judgment, so it is not a static check.
- Story tests must be at a boundary (`tests/`, P4's rule); `code plan check` reports one in `src/` as a finding.

### E4. Graph schema and watched queries (todo-102)

What and why: the joined graph (planned, actual, changes, runs, dossier) has rules that a person or an agent must
not have to remember. Each rule is data: the fields an entity needs, a check, and an action.

The schema is `planning/graph.schema.jsonc` (versioned) over the built-in defaults:

```jsonc
{ "entities": {
    "work_item": { "required": ["text", "stories", "units"], "optional": ["depends_on", "status"],
                   "checks": [ { "check": "has_tests", "action": "finding:warn" } ] },
    "story":     { "required": ["text", "tests"], "optional": ["variations"],
                   "checks": [ { "check": "variations_covered", "when": "done", "action": "finding:error" } ] },
    "test":      { "checks": [ { "check": "runnable", "action": "finding:warn" },
                               { "check": "run_since_change", "action": "run:tests" } ] } },
  "relations": {
    "change→work_item": { "checks": [ { "check": "planned", "action": "message:hint" } ] },
    "plan_row→feature": { "checks": [ { "check": "status_matches", "action": "finding:error" } ] } } }
```

- `check` is a name from a closed set in legionctl (`resolves`, `exists_at_tree`, `has_tests`,
  `variations_covered`, `runnable`, `run_since_change`, `killed`, `boundary`, `planned`, `status_matches`). A new
  check is code, with its own e2e test.
- `action` is a closed set: `finding:<info|warn|error>` (doctor, `dossier check`), `message:<note|hint>` (to the
  main agent, through B3), `run:<checks|tests|graph>` (a job: a check of E2, the linked tests, a graph refresh). No
  action runs a free command; a command is always a named check of E2.
- `code plan check` applies `required` and the checks; it replaces the fixed rules of P3 where a rule is the same.

Queries. A query is a named read that gives diagnostics:

```jsonc
// <repo>/.legionctl/queries/changed-not-run.jsonc  (or <dossier home>/queries/)
{ "name": "changed-not-run", "on": ["change", "stop"], "severity": "warn",
  "sql": "SELECT item, test FROM v_item_tests WHERE last_run < item_changed",
  "message": "{item}: {test} did not run since the change", "action": "message:hint" }
```

- `sql` is one `SELECT` on a read-only connection (`PRAGMA query_only`) over documented views: `v_units`,
  `v_items`, `v_tests`, `v_item_tests`, `v_changes`, `v_change_entities`, `v_check_runs`, `v_plan_rows`,
  `v_artifacts`. The views are a contract with a version; `code queries check` refuses a query on an unknown view or
  column. The operator fills the views from `planning/` at the tree, the graph snapshot, and the `dossier.db` files.
- `on` is a set of triggers: `change` (E5), `stop`, `checkpoint`, `tests.run`, `checks.run`, `session-start`.
- Built-in queries (shipped, can be turned off per repository): `changed-entity-not-planned`, `changed-not-run`,
  `story-variation-not-covered`, `test-planned-not-written`, `test-not-runnable-here`, `check-kind-missing`,
  `done-row-open-item`, `dossier-edited-outside-tools`, `item-done-not-releasable`.
- Each result row is one diagnostic with an id (`query` + row key). Table `diagnostics` keeps first seen, last seen
  and closed (with the change that closed it), so a reader sees new, open and closed ones.
- Readers: `doctor` (group Agents, open errors), `dossier check` (all open ones of the dossier), the observer's
  watch rules (a new diagnostic is an event `code.diagnostic`; a rule may wake the observer for it).

API (group `code`): `code queries list` (each query, source file, last result; read), `code queries run [<name>]
[--on <trigger>]` (read), `code queries check` (read), `code diagnostics [--open] [--feature]` (read).

### E5. Change watcher (todo-103, second part; decision 4)

What and why: edits happen outside legionctl's tools (an editor, a script, an agent's own write). The watcher sees
each one, so each change gets a record, a hash and its test state, also between checkpoints.

Split of work (the root daemon never reads or writes user content):

| Part | Who | Does |
| --- | --- | --- |
| Watch | `legionctl-system` (root) | fanotify on the registered roots; the path, the event, the writer's pid, `comm` and cgroup |
| Record | the user's operator | reads the files as the user, makes the change records, the hash and the test state |

Daemon:

1. fanotify with `FAN_MARK_FILESYSTEM` and `FAN_REPORT_DFID_NAME` (needs `CAP_SYS_ADMIN`, so the root daemon), events
   `CREATE`, `DELETE`, `MOVED_FROM`, `MOVED_TO`, `CLOSE_WRITE`. This is why root: one mark per file system covers
   new folders with no recursive setup, and each event has the writer's pid, so the change has its session (the pid's
   cgroup is the legionctl session scope).
2. Only registered roots: an event outside them is dropped in the daemon. The daemon resolves each event's folder
   handle to a path (`open_by_handle_at`, a cache keyed by handle) and drops paths under each root's ignore list.
3. Registration over D-Bus: `Watch(roots as, ignore as)`, `Unwatch(roots as)`. The daemon accepts a root only when
   the caller's uid (`GetConnectionUnixUser`) owns it (`O_PATH|O_NOFOLLOW`, `fstat`). polkit action
   `org.legionctl.watch`, allowed for the active user without a prompt; the owner check is the guard.
4. Delivery: the daemon appends events to `watch_events` in `system.db` (uid, seq, root, path, event, pid, comm,
   cgroup, time), bounded per uid (`watch.max_events`, 100 000). It sends the unicast signal `ChangesReady(seq t)` to
   the registering connection only (other users do not see paths). The operator reads with `Changes(after t) →
   (events, last t)`; its cursor is in `operator.db`.
5. Restart: `watch_roots` (uid, root, ignore) are in `system.db`. A daemon start marks them again and adds one
   `rescan` event per root (the gap is unknown). A queue overflow (`FAN_Q_OVERFLOW`) or the bound adds `rescan` too.
   The operator answers `rescan` with a full compare (E5 rule 9). A file system without fanotify FID support
   refuses the root with the reason; `doctor` names it.

Operator:

6. Roots: the folder `~/agentWorkspaces`, and for each dossier each repository in `entities` (main checkout) and
   each open workspace path. It registers at start, at `workspace open/close`, at a dossier change of `entities`.
   Ignore: `.git/`, `.graf/`, `.state/`, the `agentWorkspaces` link (the home is its own root), and the top-level
   ignored folders of the repository (`git ls-files --others --ignored --exclude-standard --directory`), sent again
   when a `.gitignore` changes.
7. A change closes at the writer session's `stop` hook (its edits of that reply), or after `watch.quiet` (5 s)
   with no event for edits with no session. One change has one root.
8. A change record (`changes`: id, hash, dossier, root, session, member, from tree, to tree, opened, closed):
   - Repository root: the working tree's hash through a private index (`GIT_INDEX_FILE=<tmp> git add -A`, then
     `git write-tree`); the user's index does not change. Entities: each path with `change` and blob id (`git diff
     --raw <from tree> <to tree>`), and each planned unit that the paths address (`file:`, `unit:`, and symbol units
     by their file). Symbol entities come from `code graph changes <from tree> <to tree> --build` in a job after
     `stop`; they go in `change_entities` but not in the hash, so the hash is known at once.
   - Dossier root: entities are plan rows (`plan#<id>`) and artifacts (front-matter id) with the hash of their text.
     A status that changed in an editor goes into `status_changes` with `by: outside`, which closes the gap of 18 ("a
     change made outside legionctl is not seen").
   - Hash: BLAKE2b-128 over the sorted lines `<path>\t<change>\t<blob id>` and `<unit key or node id>\t<work item
     or row>`. Two dossiers that record the same main-checkout change get the same hash; union reads keep one.
   - An edit in a workspace goes to its dossier. An edit in a main checkout goes to the writer session's dossier,
     else to each dossier that attaches the repository, as an `outside workspace` diagnostic.
9. Rescan: the operator compares the working tree's hash with its last record (repositories) and the text hashes of
   the nodes (dossier homes), and records the difference as one change marked `rescan`.

Test state per member and per work item (computed at read, from `changes`, `change_entities`, E3 and `test_runs`):

| State | Rule |
| --- | --- |
| `not defined` | the work item's stories have no test, or a story with variations has one not covered |
| `planned, not written` | a test key is planned and not at the tree |
| `written` | the test was added or changed in a change of the work item (test authored) |
| `not run since change` | the test's last run is older than the newest change that touches the item, or is at a tree without it |
| `not runnable here` | E3 |
| `passed` / `failed` since change | a run at a tree that has the change |

A member (an entity or a path) that no work item addresses is `unplanned` (the query `changed-entity-not-planned`).
"How untested a feature is": `code plan status` gets per work item and per feature the changed members since
`--since` (default: the workspace base) and how many of them have each state; `untested` is the share of changed
members whose tests are not `passed since change`, with the list.

API (group `code`): `code watch status` (roots, the daemon's state, the cursor lag, the last change; read), `code
watch changes [--since] [--session] [--feature] [--root]` (change records with entities and hash; read), and the
new fields of `code plan status`. Events: `code.change` (one per closed change), `code.diagnostic`.

### E6. Observer: tools on time, backfill and the recovery brief (todo-104; decision 3)

What and why: the main agent forgets reads and writes of the dossier, the plan, the tests and the release steps, and
it loses its state at a compaction. The operator finds each miss without a model; the observer acts only on the
misses that need it, while the main agent stays on the critical path (B+0).

Tools on time. Default watch rules of the `thread-observer` archetype (B3 form: trigger, expect, window), shipped as
data in its config defaults:

| Tool area | Trigger | Expected within the window |
| --- | --- | --- |
| dossier read | `session-start` (also after a compaction) | a read of `plan.md` or `dossier brief` before the first edit |
| planning read | the first edit of a planned unit in a session | `code plan status --feature <f>` |
| dossier write | a checkpoint; a failed test or check run | a ledger write in the same reply |
| dev | a change that touches a work item | the `lint` and `format` checks before `stop` |
| test | a change that touches a work item | its tests run before `declare done` (`not run since change` is empty) |
| test authoring | a change of a work item with `not defined` or `planned, not written` tests | a test of that item in a later change of the same reply set |
| release | `workspace merge`, `declare ready` | each `gate: merge` check passed at the head tree; `code graph changes` from the merge base after a rebase (A8) |

What the main agent is told (B3 kinds, batched at the next prompt after a `stop` or `declare`, B+0):

| Finding | Kind | Example |
| --- | --- | --- |
| miss | `reminder` with the count | "observer0: no `lint` check after 3 changes of W2 — 2nd miss since 10:42" |
| deviation | `hint` | "change 4f1c… edits `codegraph::prune`, which no open work item plans" |
| drift | `hint` | "plan row todo-101 says done; W3 has 2 tests not run since change 9a2e…" |
| backfill | `notice` | "observer0 wrote for you: plan.md row done-41 (checkpoint c48fa60), EVIDENCE/validation/13-checks (run 812)" |

Backfill (replaces B+ decision 5):

1. What: a ledger row (append only), an EVIDENCE artifact of a run that the observer did (a new file), a status
   change that a real result supports (a passed `gate: merge` set and `complete` items for `done`; a failed run for
   `failed`).
2. Mark: a row ends with `(by observer0)`; an artifact has `by: observer0` in its front matter; a status change
   writes only the status cell, `done (by observer0)`, and `status_changes` keeps the old value and `by`.
3. Never: an edit or a delete of another cell, another line or another artifact of the main agent.
4. When: only while the main agent's state is `idle` (B+1), through the dossier lock as a member write
   (`dossier_* --member observer0`, allowed only for a `thread-observer` member of the thread whose dossier it is);
   the anchored edit refuses an outdated anchor, so a write that races the main agent fails and waits for the next
   idle window.
5. Record: each backfill is a row in `backfills` (path, anchor, what, the result it rests on) and an effect (B2), so
   `thread compensate` removes it; the next prompt gets one `notice` that lists them.

Recovery brief. After a compaction the main agent gets its state back from data, not from a model:

- Hook: `session-start` with source `compact`, `resume` or `clear` (Claude Code's `SessionStart` matchers) adds the
  brief as `additionalContext`; `dossier brief` gives it on request. The operator builds it; no observer wakes.
- Sources: the plan rows of the dossier, `planning/` at the workspace's working tree, `code plan status`, the open
  changes and their test state (E5), the last check runs (E2), open diagnostics (E4), pending messages (B4).
- Each work item of each feature that the dossier's plan rows name gets one class:

| Class | Rule |
| --- | --- |
| `active` | in progress, or has a change since the last checkpoint |
| `pending` | planned and each `depends_on` item is complete |
| `blocked` | a `depends_on` item is open, a failed check or run, or an open hard block |
| `releasable` | complete, its tests `passed since change`, each `gate: merge` check passed at the head tree, its plan row not `done` yet |
| `done` | complete and its plan row `done` |

- Form: one line per feature (`code-graphs: active W5; pending W6; releasable W4`), then the open items with the
  next step of each, then the misses and backfills since the compaction. `brief.max_tokens` (1500) limits it; the
  rest is one `dossier brief --feature <f>` line. The tokens count in the cost to the main agent (B+4).
- The observer wakes only when a rule finds work for a model in the brief (for example a drift that needs a
  judgment), at its normal triggers (B5).

API: `dossier brief [--feature]` (group `dossier`, read: the brief belongs to the dossier and works without a
thread), the `--member` option of the dossier write operations (session), and `backfills` in `thread show`.

### E7. Storage, failure and restart

| Data | Place |
| --- | --- |
| Checks, schema, queries of a repository | `<repo>/.legionctl/checks.jsonc`, `planning/graph.schema.jsonc`, `<repo>/.legionctl/queries/` (versioned) |
| Runs, changes, diagnostics, backfills, tests, annotations | `<home>/.state/dossier.db`; logs and profiles in `<home>/.state/runs/` |
| Watch roots and the event queue | `/var/lib/legionctl/system.db` (root; paths and pids only, no content) |
| Cursor, dossier registry, bindings, sessions, workspaces | `~/.local/state/legionctl/operator.db` |
| Graph snapshots | `.graf/refs/` (decision 2, unchanged) |

| Event | Behaviour |
| --- | --- |
| Daemon restart | Roots marked again from `system.db`; one `rescan` per root; the operator compares. |
| Operator restart | It reads from its cursor; open changes close by `watch.quiet`; jobs resume (check runs restart from the start). |
| Queue overflow | `rescan`; `doctor` shows the count of overflows. |
| Dossier folder renamed | The id stays; the operator unregisters the old root and registers the new one. |
| Migration cut off | The user runs `legionctl dossier migrate` again; `~/.agentworkspace` stays until step 6 passes. |
| A check hangs | `timeout` per check (default 30 min); the unit is stopped; the run is `timeout`. |

### E8. Tests (e2e, high diagnostic value)

Real operator and a real `legionctl-system` on a private bus (as the machine tests), temporary homes and
repositories; stand-ins only where a tool is not the subject.

| Test | It catches |
| --- | --- |
| `the_dossiers_move_to_their_homes_once_and_keep_their_ids_links_and_rows` | A row in the wrong `dossier.db`; a worktree link left on the old path; a second run that moves again; removal before the counts match; a rename that loses bindings. |
| `the_first_use_in_a_repository_imports_its_agentworkspaces_folder_and_links_the_home` | A first-level file taken as a dossier; a name that exists in the home overwritten (must be `-imported-1`); the source removed before each copy is registered; a cut-off import that loses or copies twice; `.gitignore` with `agentWorkspaces/` (does not ignore a link); `.gitignore` committed, not staged; a worktree where `git check-ignore` fails; a plain folder given a `.gitignore`. |
| `the_watcher_records_an_editor_change_with_its_session_hash_and_untested_items` | A change with no session when the writer runs in a session scope; a hash that differs for the same content; the user's index changed by the record; a work item not `not run since change` after its edit; a test run that does not clear it. |
| `the_daemon_watches_only_owned_registered_roots_and_survives_a_restart` | A root of another uid accepted; a path outside the roots delivered; the signal sent to another connection; an edit during a daemon stop not found (`rescan`); a file written by root. |
| `a_checkpoint_and_a_merge_run_the_repositorys_gates_and_keep_their_profiles` | A gate skipped with no run at the tree; a passing run at another tree accepted; no memory peak in the profile; a missing kind not a finding; `detect` writing a file. |
| `planned_actual_and_runnable_tests_differ_and_variations_need_their_tests` | An `#[ignore]` test shown runnable without its probe; a planned test shown as written; a `done` story with an uncovered variation accepted. |
| `a_query_on_watch_opens_and_closes_its_diagnostic_and_wakes_no_model` | A diagnostic opened twice; not closed by the change that fixes it; a query on an unknown view accepted; a model wake for a diagnostic that needs none. |
| `the_observer_backfills_only_while_main_is_idle_and_tells_main_once` | A write while main runs; an edit of main's text; a missing `by observer0`; the notice given twice or not at all; `thread compensate` not removing it. |
| `after_a_compaction_the_brief_classes_each_item_from_data` | A `releasable` item with a test not run since change; an `active` item without a change; a brief past its token limit. |

### E9. Phases

| Phase | Content | Size (new lines, estimate) | Needs |
| --- | --- | --- | --- |
| P9 | Dossier home, ids, the registry, the SQLite split, the migration, the directory link and the import (E1a), skill and doc text (`~/.agentworkspace` everywhere) | M (900) | P5 merged (its tables move) |
| P10 | Project checks: config, `detect`, runs with profiles, gates, findings and nudge; `code tests run` uses the `test` checks | M (700) | P9 |
| P11 | Watcher: daemon fanotify, D-Bus, `system.db` queue; operator roots, change records, hash, test state, `code watch`, untested in `plan status` | L (1200) | P9, P10 |
| P12 | Runners and the three test sets; story variations in `planning/` and its schema | M (600) | P10 |
| P13 | Graph schema rules, queries, views, diagnostics | M (900) | P11, P12 |
| P14a | Recovery brief and its hook | S (400) | P11 |
| P14b | Observer rules of E6, backfill, notices | S (400) | P6b, P13 |

### E10. Coverage of the request (Part E)

| Phrase of the request | Section |
| --- | --- |
| passes lint/format/static/type analyses/checks and builds/deploys/installs as configured in repo | E2 |
| agent is nudged to define it in workspace config/settings correctly | E2 rules 2 and 3 |
| has a home for profiles from runs | E2 rule 5, E1 (`.state/runs/`) |
| test-runners for tests in codegraphs; planned v/s actual; runnable is a test with a runner the env can run | E3 |
| codegraph has required and optional fields, linked programmatic checks and actions for entities and relations | E4 schema |
| ootb/added queries on watch for diagnostics | E4 queries |
| all stored durably on dossier/workspace's local filesystem spaces | E1, E7 |
| home of all dossiers is `$HOME/agentWorkspaces/<name>`, names mutable but unique; all the sqlite there | E1 (decision 1), E7 (graphs stay in `.graf`: decision 2) |
| a link to the home in each directory root; import of a real `./agentWorkspaces/`; gitignore staged and checked | E1a |
| daemon watches dossiers and attached repos (watcher on fs), records edits and diffs | E5 (decision 4) |
| a unique hash per change from plan/repo-graph addressed content and paths | E5 rule 8 |
| tests run / authored / not ready or defined; not run for a member or a work item | E5 test state |
| how untested a feature is with its changes | E5 (`untested` in `code plan status`) |
| e2e grade tests across user stories, variations, branches, expected success/failure with details | E3 variations |
| observer makes sure dossier/planning/dev/test/release tools are used on time, reads and writes | E6 watch rules |
| informs main of misses, deviations, drifts, and what it backfilled | E6 table, backfill (decision 3) |
| gets main back on track after compactions; active/pending/releasable across work items x features | E6 recovery brief |

### E11. Questions for the user

1. Where do the rows that span dossiers live (bindings, sessions, workspaces, the id registry)? Recommendation:
   `~/.local/state/legionctl/operator.db`, the operator's own database; each dossier home then holds only its own
   rows.
2. Kills, runs and annotations count for a repository, but a dossier home owns each row. Recommendation: write into
   the writer's dossier and read the union of the dossiers that attach the repository (so a kill stays evidence
   for all of them, as P4 decided).
3. Where does a repository declare its checks? Recommendation: the config key `checks` in the repository layer
   (`<repo>/.legionctl/checks.jsonc`, versioned), so a branch can change them and `config explain` names the file.
4. How strong are the check gates? Recommendation: `workspace checkpoint` runs the `gate: checkpoint` checks
   (format, lint) when no passing run exists at that tree; `workspace merge` refuses without passing `gate: merge`
   checks (build, test) at the rebased head.
5. How does the root daemon watch? Recommendation: fanotify per file system with the writer's pid (the session of
   each change), only for roots that the caller owns, events queued in `system.db` and read by the user's
   operator; the daemon never opens user content. No inotify path.
6. Query language for watched queries? Recommendation: one read-only SQL `SELECT` over versioned views, with
   actions from a closed set (finding, message, a named check or test run); no free commands.

## Decisions on Part E (user, 2026-10-15)

1. Rows that span dossiers (bindings, sessions, workspaces, the id registry) live in the operator's own database,
   `~/.local/state/legionctl/operator.db`; each dossier home holds only its own rows.
2. Repository facts (kills, runs, annotations) are written into the writer's dossier and read as the union of the
   dossiers that attach the repository.
3. A repository declares its checks in `<repo>/.legionctl/checks.jsonc` (config key `checks`, repository layer).
4. Gates: `workspace checkpoint` runs the `gate: checkpoint` checks (format, lint) when no passing run exists at that
   tree; `workspace merge` refuses without passing `gate: merge` checks (build, test) at the rebased head.
5. The root daemon watches with fanotify (writer's pid, only roots the caller owns, events queued in `system.db`, read
   by the user's operator; the daemon never opens user content). A live check of FID support on this machine's file
   systems comes first in its phase.
6. Watched queries use ctx's graph query form, over the code graph only. Diagnostics over the plan, runs and tests
   come from the built-in checks (`dossier check`, `code plan check`, the watch rules), not from added queries; E4's
   SQL views are not built.

7. Dossier names (the user, same day: "agents runs are like projects; unique" is for dossier names): a dossier name
   is descriptive, unique and may be changed at any time, like a project name; the stable id in `dossier.jsonc`
   keeps every reference (bindings, links, events) valid across renames. A rename to a name in use is refused.
