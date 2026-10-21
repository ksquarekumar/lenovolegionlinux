---
description: The dossier's workspace (git worktree) - open, status, checkpoint, merge, close; topic workspaces for parallel agents
argument-hint: open [repo] [--base <ref>] [--topic <topic>] | status | checkpoint "<message>" | merge | merge-request | close [--force] (each with [--topic <topic>])
---
Manage this session's dossier workspace. Run each operation with the legionctl MCP tool `legionctl_run`: $ARGUMENTS

Follow skill `legionctl:workspace-practice`; it has the rules for each step.

1. `open [repo] [--topic <t>]`: run `workspace.open` (repo default: the current project). Work in the returned `path`.
2. With a topic, give the same `topic` to each later workspace operation.
3. `status` or no arguments: run `workspace.status`. Show ahead and behind, changes and drift.
4. `checkpoint "<message>"`: run `workspace.checkpoint` with the message of the plan entry.
5. `merge`: ask the user, then run `workspace.merge`.
6. `merge-request`: when `workspace.merge` refuses a branch that changes `.legionctl/`, run `workspace.merge-request`.
   The user allows or denies it in a desktop dialog that shows the diff.
7. `close`: run `workspace.close`. If it refuses, show the uncommitted changes and ask before you pass `force`.
