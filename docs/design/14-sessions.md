# 14 — Sessions, tools, model calls and the config model

Requirements (2026-10-12): the harness packages skills (npx and others) and MCP servers (stdio and remote), proxies
and caches their calls; it creates ACP sessions with their whole environment and exposes them like a model call,
against an account with quotas and tracking; each session has a durable record (operating directory as a
copy-on-write copy, active MCP servers/tools/skills, git state, permissions) and can run, pause or be offloaded and
resumed quickly; mutating git and other unsafe commands are gated by a layered config whose base data lives in a
privileged location.

Builds on 12-operator (harness, sandbox, gateway) and 13-daemon (one-writer database, event log, sagas). Owner:
`legionctl-operator` (user); privileged parts through `legionctl-system`.

## Built (2026-10-13, milestone 1: interactive agents in a sandbox)

- `legionctl session start claude|codex [--dossier X] [--create]` binds the dossier to a new session id (Claude Code
  gets it as `--session-id`, so its hooks find the binding), opens the dossier's workspace (18), records the session
  in `workspace.db`, and runs the agent in the terminal: `systemd-run --user --scope` (unit
  `legionctl-session-<id>`), then `bwrap` with `/` read-only (your home too, decided 2026-10-13), a private `/tmp`,
  and writable: the workspace, the dossier, the repository's `.git`, the runtime folder and `session.writable`
  (`~/.claude`, `~/.claude.json`, `~/.codex`, `~/.cache`). The network stays on: agents need their model API.
- The git gate: `/usr/bin/git` in the sandbox is the `legionctl` binary; started as `git`, it asks the operator
  (`gates.check`, config `gates.git.deny`, lockable from `/etc`) and runs the real git (mounted read-only in the
  session's runtime folder) or refuses. Default (decided 2026-10-13): local commands run; push, fetch, pull, remote,
  reset --hard, rebase, clean -f, branch -D, filter-branch, gc --prune, update-ref and reflog expire are refused;
  git aliases are refused, because an alias could name a denied command.
- `session list` (state from the scope), `session stop`. Checked live: the scope, the refused push, the read-only
  home. Not yet: agent permission generation (layer 1 below), overlay copies for plain folders, ACP
  (pause/offload/resume, sessions as model calls), the tool proxy and cache.

Built (2026-10-13, S2a): agent permission generation. `session start` writes
`$XDG_RUNTIME_DIR/legionctl/sessions/<id>/claude-settings.json` with each `gates.git.deny` rule as a Claude Code
`permissions.deny` entry (`Bash(git push:*)`), and the `claude` template passes it with `--settings {settings}`. The
e2e session test reads the file from inside the sandbox. Codex keeps its command: its own sandbox inside bwrap is
still to be checked live.

Built (2026-10-13, S2b): the MCP proxy. `mcp.servers` (config) names each server's command and tool allowlist.
`session start` writes `sessions/<id>/mcp.json`: legionctl's own server, and each configured server as
`legionctl mcp-proxy --server <name> --allow <tool>... -- <command>`; the `claude` template passes it with
`--mcp-config {mcp} --strict-mcp-config`, so the agent has no other servers. The proxy (rmcp server to the agent,
rmcp client to the server it starts) lists only the allowed tools, refuses others, and logs each call through the
operator (`tools.record`, event `tool.call`: session, server, tool, BLAKE2b-128 of the arguments, ms, bytes,
outcome ok/error/failed/refused); `legionctl tools calls [--session]` lists them. A log that fails (operator down)
does not stop the call; the proxy says so on stderr.

Built (2026-10-13, T1): the package store. `packages` (config) gives each package a kind (npm, pypi, cargo, git,
path), a source, a pin (`version`), its server's `run` line (`{dir}` = its folder) and `network` (default false).
`legionctl packages fetch <name>` installs it into `~/.local/share/legionctl/store/<name>/<version>/` inside bwrap
with network but a scratch home and /tmp (npm with `--ignore-scripts`; git without its history), then records a
manifest with the BLAKE2b-256 hash of the tree; `legionctl packages list` checks every hash again. An MCP server
can name a package: `session start` checks the hash (a changed package stops the session before a dossier is bound)
and writes the store command into `mcp.json`; the proxy runs it in `bwrap --unshare-net` unless the package allows
network. The e2e test fetches a git package, runs it through the generated proxy command (only loopback, only the
allowed tool), changes a store file and sees the refusal. Not yet: skills from the store (Claude Code reads skills
from the real home), remote servers with keyring credentials.

Built (2026-10-13, T2): the call cache. `mcp.servers.<name>.cache` lists the tools whose results may be reused,
each with `ttl_s` and `workspace` (the tool reads the working copy). Nothing else is cached; a tool error is never
kept. The proxy's key is a BLAKE2b-128 of the server's command (for a package, its store version), the tool, the
arguments and, with `workspace`, the git state of its folder (HEAD, `status --porcelain=v2`, `diff HEAD`). Entries
live in the operator database (`tool_cache`, migration v3) through `cache.get` and `cache.put`; a hit is logged as
outcome `cached`. The e2e test sees ok, cached, then ok again after a file in the working copy changes.

Built (2026-10-13, S3): headless runs. `legionctl session run <agent> <prompt>` prepares the session as `session
start` does (dossier binding, workspace, sandbox with the hiding and the git gate, generated MCP config), then the
operator drives the agent's ACP adapter (`session.acp`: Zed's `claude-code-acp` and `codex-acp` by default) with
adk-rust's ACP client (`adk-acp` 2.2.0, protocol v1). The adapter gets the session's MCP servers at `session/new`.
Permission asks are allowed, because the sandbox and the gates are the boundary; each is logged as
`session.permission` and returned. `session.scope` (default true) runs it in the systemd user scope. The e2e test
drives a stand-in ACP agent (`tests/fixtures/fake-acp-agent.py`): it gets the MCP servers, its permission is
answered, its commit lands in the workspace and the gate refuses its push. Not yet: streaming the answer while it
runs, the real adapters live (they need the user's logins).

Built (2026-10-13, S4): `session pause|thaw <id>` freeze and thaw the session's scope (cgroup freezer;
`systemctl --user freeze|thaw`), `session stop <id>` stops it, `session resume <id>` prepares an ended session
again (same id, dossier binding and workspace; the sandbox anew) with the agent's resume command (`session.resume`:
`claude --resume {id} ...`; Codex has none by id). The e2e test resumes a session and sees the same id and folder,
and a refusal for an agent without a resume command; pause and thaw were checked live (active, frozen, running,
inactive), as CI has no user systemd. Not yet: the idle and memory-pressure policies that pause and offload on
their own, and ACP `session/load` for headless runs.

Fixed (2026-10-13): the sandbox bound the whole `$XDG_RUNTIME_DIR`, and `--ro-bind / /` does not stop a connect to a
socket. The agent could reach the session bus and systemd's user manager (`systemd-run --user` runs outside the
sandbox, past the git gate), the GnuPG, ssh and keyring agents, the docker socket (`docker` group: a container with
the host's files) and libvirt. Now the runtime folder is a private tmpfs with only `legionctl/` bound back, and
`session.hidden` (default: docker, containerd, podman and libvirt sockets) puts an empty tmpfs on a folder and
`/dev/null` on a file or socket. Package fetches (`cargo install` runs build scripts) use the same hiding. The session test checks a decoy session bus and a listening socket from inside;
its runtime folder is under /var/tmp like /run/user (under /tmp the sandbox hid it anyway, and the test passed
without the fix).

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

Built 2026-10-13 (`legionctl-core::config`, operations `config.show`, `config.explain`, `config.layers`). Layers,
each a folder of `*.jsonc` files merged in name order:

| Layer | Path | Writable by |
| --- | --- | --- |
| defaults | `config/defaults.jsonc`, compiled into every binary (one copy, nothing to install) | the repo |
| system | `/etc/legionctl/` | root |
| user | `~/.config/legionctl/` (`managed.jsonc`, written by legionctl, merges last) | user, legionctl |
| repository | `<repo>/.legionctl/`, for work inside that repository | repository commits |
| session | session record overrides (with sessions) | harness, per session |

- **Format:** JSONC, like the machine profile (decided 2026-10-13).
- **Merge:** per key only (decided 2026-10-13): objects merge per key, other values replace, `null` removes a key,
  `"name+": [...]` appends to the array `name`. A whole-file merge mode waits for a reader that needs it.
- **Order and locks** (`config.precedence`, `config.locked`) come only from the defaults and `/etc/legionctl`. A
  lower-trust layer that sets `config` or a locked key (or a key below it) is ignored and reported, so a repository or
  session cannot reorder layers to escape a gate. `legionctl reminders` and `legionctl hooks` refuse to write a
  locked key.
- **Explain:** `legionctl config explain <key>` shows the value, whether it is locked, and every file that set it or a
  key below it, lowest first. Each session will store its resolved snapshot, so later config edits do not change
  running sessions until they are resumed or explicitly re-resolved.
- **Readers today:** `worktree.root`, `reminders.*` (built-in and your own), `hooks.actions` and `hooks.bindings`
  (the operator database no longer holds them; migration v2 drops its hook tables), `load.*` (the load classifier's
  thresholds).
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
