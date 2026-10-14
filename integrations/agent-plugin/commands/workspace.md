---
description: The dossier's workspace (git worktree) - open, status, checkpoint, merge, close; topic workspaces for parallel agents
argument-hint: open [repo] [--base <ref>] [--topic <topic>] | status | checkpoint "<message>" | merge | close [--force] (each with [--topic <topic>])
---
Manage this session's dossier workspace with the legionctl MCP tools: $ARGUMENTS

Follow skill `legionctl:workspace-practice`; it has the rules for each step.

1. `open [repo] [--topic <t>]`: call `workspace_open` (repo default: the current project). Work in the returned `path`.
2. With a topic, give the same `topic` to each later workspace tool.
3. `status` or no arguments: call `workspace_status`. Show ahead and behind, changes and drift.
4. `checkpoint "<message>"`: call `workspace_checkpoint` with the message of the plan entry.
5. `merge`: ask the user, then call `workspace_merge`.
6. `close`: call `workspace_close`. If it refuses, show the uncommitted changes and ask before you pass `force`.
