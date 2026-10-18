---
name: tool-lifecycle
description: How every legionctl command runs (operator, permission classes, events, MCP tools), the order of a task, and an index of the command groups with the skill that owns each. Use when you do not know which legionctl command fits a task, or before you use a command group for the first time.
user-invocable: true
---

# legionctl tool lifecycle

1. The `legionctl` client sends each command to the operator (`legionctl-operator`), your user service on
   `$XDG_RUNTIME_DIR/legionctl/api.sock`. systemd starts it at the first call. When the user turned it off, each
   command says "legionctl-operator is off" with the line that turns it on; give the user that line.
2. Each operation has a permission class (`docs/api/schema.json`). `read` runs at once and changes nothing.
3. `session` changes only the caller's own session: its dossier, and its workspace except merge. It runs at once.
4. `workspace` changes your files, settings, containers or other sessions. A sandboxed agent's call waits for the user:
   a desktop notification with Allow and Deny, 120 s. No answer is a refusal. Outside a sandbox, an agent's change of
   hooks or reminders waits the same way, and only the user answers a question (`approval answer`).
5. A repository's `.legionctl` files and its compose file are used only after the user allows their content once
   (the same desktop question). Until then the call says the file "is not used until" the user allows it; tell the user.
   Change the user's settings and answers with legionctl, not with a file write: the pre-tool hook denies it.
6. `machine` goes to the root daemon (`legionctl-system`) with a polkit dialog. See skill machine-tools. When the
   daemon is off, the call says "legionctl-system is off"; `legionctl setup turn-on legionctl-system` starts it
   (workspace class; polkit asks).
7. Each call that is not a read is an event (kind = the operation name) with the actor, the input and the result.
   `events` pages the log (`--cursor`, `--limit`); `trace` follows it live. The app is the only UI;
   agents and scripts read its views with `views render` (text or `--json`).
8. Long operations are jobs: `jobs list`, `jobs show <id>`. See skill sessions-and-agents.
9. Over MCP, three tools reach every operation: `legionctl_find` (by words or group), `legionctl_describe` (help,
   input schema, class) and `legionctl_run` (`op` is the command path with `.`: `kernel.cmdline.set`).
10. `--json` prints the operation's JSON output. `--dry-run`, where a command has it, shows the plan and changes nothing.
11. Read `legionctl <group> --help` before you use a flag. Do not guess.
12. Check `legionctl load` before builds, test suites and parallel runs. If the machine is busy, run less.
13. Each agent tool that you add or change (an operation's output, an MCP tool, CLI output for agents, a hook or
    skill text) is token-efficient and task-efficient: one call answers one task; counts and stable keys first, one
    line each, detail through a cursor; a `max_tokens` budget that says what it left out; deltas since the last call;
    keys, not pasted content. This rule is binding for all tooling (the user, 2026-10-16).

## Order of a task

1. Bind a dossier (skill dossier-practice).
2. Open a workspace (skill workspace-practice).
3. Read the machine when the task needs it (skill machine-tools).
4. Give work to a session when it must be isolated or headless (skill sessions-and-agents).
6. Verify (skill verify-and-ship).
7. Checkpoint, then merge with the user's approval (skill workspace-practice).

## Refs

- `refs/reference-command-groups.md` - read to find the group, class and owner skill of a command.
- `refs/reference-thread-screen.md` - read before you place a view or answer in the app: its tabs and `/` commands.
