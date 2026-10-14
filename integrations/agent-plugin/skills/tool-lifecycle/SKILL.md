---
name: tool-lifecycle
description: How every legionctl command runs (operator, permission classes, events, MCP tools), the order of a task, and an index of the command groups with the skill that owns each. Use when you do not know which legionctl command fits a task, or before you use a command group for the first time.
user-invocable: true
---

# legionctl tool lifecycle

1. The `legionctl` client sends each command to the operator (`legionctl-operator`), your user service on
   `$XDG_RUNTIME_DIR/legionctl/api.sock`. systemd starts it at the first call.
2. Each operation has a permission class (`docs/api/schema.json`). `read` runs at once and changes nothing.
3. `session` changes only the caller's own session: its dossier, and its workspace except merge. It runs at once.
4. `workspace` changes your files, settings, containers or other sessions. An agent's call waits for the user: a desktop
   notification with Allow and Deny, 120 s. No answer is a refusal.
5. `machine` goes to the root daemon (`legionctl-system`) with a polkit dialog. See skill machine-tools.
6. Each call that is not a read is an event (kind = the operation name) with the actor, the input and the result.
   `events` pages the log (`--cursor`, `--limit`); `trace` follows it live; `tui` is a live dashboard.
7. Long operations are jobs: `jobs list`, `jobs show <id>`. See skill sessions-and-agents.
8. Each operation is also an MCP tool: the command path with `_` (`kernel cmdline set` is `kernel_cmdline_set`).
9. `--json` prints the operation's JSON output. `--dry-run`, where a command has it, shows the plan and changes nothing.
10. Read `legionctl <group> --help` before you use a flag. Do not guess.
11. Check `legionctl load` before builds, test suites and parallel runs. If the machine is busy, run less.

## Order of a task

1. Bind a dossier (skill dossier-practice).
2. Open a workspace (skill workspace-practice).
3. Read the machine when the task needs it (skill machine-tools).
4. Give work to a session when it must be isolated or headless (skill sessions-and-agents).
5. Verify (skill verify-and-ship).
6. Checkpoint, then merge with the user's approval (skill workspace-practice).

## Refs

- `refs/reference-command-groups.md` - read to find the group, class and owner skill of a command.
