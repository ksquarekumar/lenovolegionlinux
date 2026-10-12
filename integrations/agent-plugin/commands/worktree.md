---
description: Create, list or remove an isolated git worktree for a task
argument-hint: create <name> [repo] | list | rm <name>
---
Manage legionctl worktrees with the legionctl MCP tools, based on: $ARGUMENTS

- `create <name> [repo]`: call `worktree_create` with the repo (default: the current project's git root) and name,
  then tell the user the new path and branch (`legionctl/<name>`) and do further work for this task inside that path.
- `list` or no arguments: call `worktree_list` with `sizes: true` and show name, size, dirty state and path.
- `rm <name>`: call `worktree_rm`; if it reports uncommitted changes, show them and ask before passing `force`.
