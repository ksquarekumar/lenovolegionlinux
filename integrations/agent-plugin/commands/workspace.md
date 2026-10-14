---
description: The dossier's workspace (git worktree) - open, status, checkpoint, merge, close; topic workspaces for parallel agents
argument-hint: open [repo] [--base <ref>] [--topic <topic>] | status | checkpoint "<message>" | merge | close [--force] (each with [--topic <topic>])
---
Manage this session's dossier workspace with the legionctl MCP tools, based on: $ARGUMENTS

A workspace is the dossier's operating directory in a repository: a git worktree on `legionctl/<dossier>`, with the
dossier linked in as `.agentworkspace`. Make code changes there, never in the repository's main checkout.

- `open [repo]`: call `workspace_open` (repo default: the current project). Then work in the returned `path`.
- `open --topic <topic>`: call `workspace_open` with `topic`. This makes a separate workspace on
  `legionctl/<dossier>--<topic>` for one agent that works in parallel with others of the same dossier. Give the same
  `topic` to each workspace tool below, because the MCP server's working directory is not the topic workspace.
- `status` or no arguments: call `workspace_status`; show ahead/behind the base, changes, and any drift. For drift,
  say what happened and propose the fix (for example switch the branch back, or open the workspace again).
- `checkpoint "<message>"`: call `workspace_checkpoint` with a message from the plan entry; record the commit in the
  dossier's plan.md.
- `merge`: ask the user first, then call `workspace_merge`. On conflicts, resolve them in the workspace path, run
  `git rebase --continue`, and merge again.
- `close`: call `workspace_close`; with uncommitted changes it refuses: show them and ask before passing `force`.
