---
description: The dossier's workspace (git worktree) - open, status, checkpoint, branch, merge, close
argument-hint: open [repo] [--base <ref>] | status | checkpoint "<message>" | branch <topic> | merge | close [--force]
---
Manage this session's dossier workspace with the legionctl MCP tools, based on: $ARGUMENTS

A workspace is the dossier's operating directory in a repository: a git worktree on `legionctl/<dossier>`, with the
dossier linked in as `.agentworkspace`. Make code changes there, never in the repository's main checkout.

- `open [repo]`: call `workspace_open` (repo default: the current project). Then work in the returned `path`.
- `status` or no arguments: call `workspace_status`; show ahead/behind the base, changes, and any drift. For drift,
  say what happened and propose the fix (for example `workspace_branch`, or open the workspace again).
- `checkpoint "<message>"`: call `workspace_checkpoint` with a message from the plan entry; record the commit in the
  dossier's plan.md.
- `branch <topic>`: call `workspace_branch`.
- `merge`: ask the user first, then call `workspace_merge`. On conflicts, resolve them in the workspace path, run
  `git rebase --continue`, and merge again.
- `close`: call `workspace_close`; with uncommitted changes it refuses: show them and ask before passing `force`.
