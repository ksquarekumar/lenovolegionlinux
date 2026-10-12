# 14 — Sessions, tools, model calls and the config model

Requirements (2026-10-12): the harness packages skills (npx and others) and MCP servers (stdio and remote), proxies
and caches their calls; it creates ACP sessions with their whole environment and exposes them like a model call,
against an account with quotas and tracking; each session has a durable record (operating directory as a
copy-on-write copy, active MCP servers/tools/skills, git state, permissions) and can run, pause or be offloaded and
resumed quickly; mutating git and other unsafe commands are gated by a layered config whose base data lives in a
privileged location.

Builds on 12-operator (harness, sandbox, gateway) and 13-daemon (one-writer database, event log, sagas). Owner:
`legionctl-operator` (user); privileged parts through `legionctl-system`.

## Session record

One row per session in the user database (plus child tables), changed only through the event log:

| Field | Content |
| --- | --- |
| id, agent | legionctl id; agent (claude-code, codex, any ACP agent) and its ACP session id |
| account | account profile (12-operator) the session bills to |
| state | `running`, `paused`, `offloaded`, `archived`, `failed` |
| workdir | source path, copy path, copy method (worktree, overlay, reflink), base ref, branch |
| tools | MCP servers (pinned package or URL), allowed tools per server, skills (pinned) |
| policy | resolved config snapshot (below) with the layer that set each key |
| sandbox | bwrap profile: writable paths, network on/off, environment |
| git | last git-state snapshot (below) |
| usage | tokens, cost and calls, per model and per tool |

Creating a session is a saga (13-daemon): copy workdir, materialize tools, resolve policy, start sandbox, ACP
`initialize` + `session/new` (with `cwd` = the copy and `mcpServers` = the legionctl proxies). Each step has a
compensation, so a failed create leaves nothing behind.

## Operating directory: copy-on-write

Disks here are ext4 (`/`, `/home`, `/data`), which cannot reflink, so:

| Source | Copy |
| --- | --- |
| git repository | `git worktree` on branch `legionctl/<session>` from a recorded base ref; ignored build dirs (`target/`, `node_modules/`) overlay-mounted from the main checkout |
| other directory | overlayfs: source read-only lower layer, per-session upper layer (kernel overlayfs in the session's user namespace) |
| btrfs/xfs volume (if added later) | reflink copy or btrfs snapshot; same API |

Repo copies always record repo, base ref, branch and merge-base, so "what did this session change" is always
`git diff <merge-base>..` plus the working tree. An overlay copy's change set is its upper directory.

## Git state

Read with `git status --porcelain=v2 --branch` after each tool call that can write (from the proxy and the hooks)
and on a timer for idle sessions:

| State | Detected by |
| --- | --- |
| clean / unstaged / staged / untracked | porcelain entries |
| conflict | unmerged entries (`u` lines) |
| conflict resolved, not committed | `MERGE_HEAD`, `REBASE_HEAD` or `CHERRY_PICK_HEAD` present and no unmerged entries |
| ahead / behind base | `--branch` headers against the recorded base ref |

Snapshots go to the event log, so the UIs show history ("conflict at 14:02, resolved 14:09") as well as now.

## Skills and MCP servers: package, pin, proxy, cache

- **Package store** `~/.local/share/legionctl/store/`: each skill or stdio MCP server is fetched once (npx, uvx,
  cargo, git), pinned to a version and content hash, and recorded with its source. Fetching runs in a sandbox with
  network; running does not get network unless its policy allows it. Updates are explicit and shown as a diff.
- **Per-session set:** the session record lists the skills and servers to activate; legionctl materializes them into
  the session home (`.claude/skills`, `.codex/skills`, MCP config) so the agent sees only that set.
- **Proxy:** the agent never connects to an MCP server directly. `session/new` passes legionctl's proxy endpoints; the
  proxy starts stdio servers inside the sandbox, connects to remote ones with credentials from the keyring, applies
  the tool allowlist, and records every call (session, tool, arguments hash, duration, result size, outcome).
- **Cache:** only tools that are read-only (`readOnlyHint`) or allowlisted as deterministic. Key = server pin + tool +
  normalized arguments + workspace state (git tree hash of the copy) for tools that read files. Mutating tools are
  never cached. TTL per tool; same storage and UI as the gateway cache.

## Model calls, accounts and quotas

19-gateway.

## Lifecycle: run, pause, offload, resume

Each session runs in its own systemd user scope (`legionctl-session-<id>.scope`) with memory and CPU limits.

| Transition | Mechanism | Resume cost |
| --- | --- | --- |
| pause | cgroup freeze (`systemctl --user freeze`) | instant; memory stays in use |
| offload | ACP session closed, process stopped; record, transcript and workdir kept | `session/resume` (stable since 2026-04) or `session/load`, plus restarting MCP servers: seconds |
| archive | offload plus workdir removed after its branch is pushed or kept | recreate the copy from the branch |

- Policy (in the config model): freeze after N minutes idle, offload after M minutes or when memory pressure is
  `Busy` (load module), wake on a timer or on an incoming run. Timers are systemd user timers, so they survive
  restarts of `legionctl-operator`.
- CRIU (installed) can checkpoint a whole process tree, but agents hold TLS connections to providers and child
  processes; restore is fragile. Kept as an experiment, not the offload path.

## Gates on git and unsafe commands

Three layers, generated from the same resolved policy:

1. **Agent permissions:** Claude Code `permissions.deny/ask` and a PreToolUse hook; Codex approval policy and sandbox
   mode. Fast feedback to the agent, but the agent's own settings could be changed, so not enough alone.
2. **Sandbox shims:** inside bwrap, `/usr/bin/git` (and other gated commands) is bind-mounted to the legionctl shim,
   which checks the policy: read-only git commands pass; `commit`, `push`, `reset --hard`, `rebase`, `checkout -- .`,
   `clean`, branch deletion and similar are allowed, asked (approval queue) or denied per policy. The agent cannot
   reach the real binary.
3. **Filesystem:** `.git` of the source repo read-only; only the session's worktree gitdir writable; nothing outside
   the copy writable unless listed.

Every gated call is an event (allowed, asked and answered, denied).

## Config model

A hierarchy of layers, each either a file or a directory of files:

| Layer | Path | Writable by |
| --- | --- | --- |
| base data | `/usr/share/legionctl/defaults/` (package) | package manager |
| system | `/etc/legionctl/` | root |
| user | `~/.config/legionctl/` | user |
| repository | `<repo>/.legionctl/` | repository commits |
| session | session record overrides | harness, per session |

- **Order and merge mode are themselves config**, set only in `/etc/legionctl/legionctl.toml`: `precedence = [...]` lists
  the layers from lowest to highest, and each layer is `merge = "key"` (deep merge per key; arrays by explicit
  `append`/`replace`) or `merge = "file"` (a file in this layer replaces the same file from lower layers).
- **Locks:** `/etc/legionctl` can lock keys or key prefixes (`locked = ["gates.git.*", "sandbox.network"]`); higher
  layers that set them are rejected with the lock shown. Only the system layer controls ordering and locks, so a
  repository or session cannot reorder layers to escape a gate.
- **Explain:** `legionctl config explain <key>` shows the value, the layer and file that set it, and the values it
  overrode. Each session stores its resolved snapshot, so later config edits do not change running sessions until
  they are resumed or explicitly re-resolved.
- `/etc` (root-owned, bind-mounted read-only into every sandbox) is the trust root; the boot partition is not needed.

## Tables (user database, new migration)

`sessions`, `session_tools`, `session_skills`, `workdirs`, `git_snapshots`, `packages` (store), `tool_calls`,
`tool_cache` (or redb if large), `accounts`, `budgets`, `usage`. Config layers stay files; the database stores each
session's resolved snapshot.

## Order of work

1. Config model (layers, precedence, locks, explain) — everything else reads it.
2. Session record, workdir copies, git state; `legionctl sessions create|list|show`.
3. Sandbox + git shim + agent permission generation.
4. ACP client (agent-client-protocol crate): create, prompt, pause/offload/resume.
5. Package store and MCP proxy with call log, then the cache.
6. Gateway accounting and budgets; `/v1/runs`.
