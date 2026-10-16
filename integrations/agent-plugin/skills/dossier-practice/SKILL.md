---
name: dossier-practice
description: How to keep this session's legionctl dossier (~/agentWorkspaces/<dossier>) - binding, the plan.md ledger, numbered MEMORY, DESIGN, EVIDENCE and SPIKE artifacts, anchored edits, and the hooks that gate and nudge. Use whenever a session has a dossier bound, and before you bind one.
user-invocable: false
---

# Dossier practice

1. A dossier is your own space for one scope, outside the product's git tree: `~/agentWorkspaces/<dossier>/`. In
   each directory where a dossier is used, `agentWorkspaces` links to that home: your files are in
   `agentWorkspaces/<dossier>/`.
2. Bind it with `dossier use <name>` (class session). A new name makes a new dossier.
3. A dossier that another live session holds is refused. `--steal` takes it; ask the user first.
4. Your session writes only to its own dossier. You can read every dossier. `dossier locks` shows who holds which.
5. Until a session has a dossier, the `prompt` hook blocks each prompt except `/legionctl:dossier`.
6. `plan.md` is the ledger. It records state only, in short entries that point to the artifact with the detail.
7. Keep requested, proposed, approved, implemented and validated state separate. A proposal is not a decision.
8. Mark a change validated only when EVIDENCE for it exists.
9. Put detail in numbered artifacts: MEMORY (intake, source reads, decisions, chat context), DESIGN (intended design),
   EVIDENCE (validation output), SPIKE (disposable experiments).
10. `dossier add <CLASS>[/<sub>] <slug>` makes the next `NN-<slug>.md`, with the content from stdin or `--content`.
11. With no plan content yet, write a MEMORY document first.
12. Move an entry that grows past a few sentences to MEMORY or EVIDENCE, and leave a pointer.
13. Write chat context that later work needs into MEMORY in the same turn.
14. Update the dossier in the same turn as the finding, decision or commit.
15. Keep every section of `plan.md`. `dossier check` reports a missing section, stale EVIDENCE and dangling links.
16. `dossier read` prints each line as `<line>:<hash>|<text>`. `dossier edit` takes those anchors (`replace`,
    `insert-after`, `delete`).
17. An anchor from an outdated read is refused. Read again, then retry.
18. Use `dossier write` for a new file and `dossier append` to add to a file.
19. The `stop` hook counts replies that changed the workspace without a dossier write. Each `dossier.nudge_turns` (5)
    replies it raises a level: info, then warn (one reply stopped), then block (each reply stopped).
20. A write into your dossier resets the count. `dossier nudges [--of <session>]` shows the levels and answers.
21. Each dossier has a stable id in `dossier.jsonc`. `dossier rename <name> <to>` changes its name (class workspace:
    ask the user first). The id, the bindings, the workspaces and the `planning/` links stay valid.
22. A rename to a name in use is refused. Choose a descriptive name that is not in `dossier list`.
23. The first dossier use in a directory links the home there as `agentWorkspaces`, and imports a real
    `agentWorkspaces/` folder first. In a main checkout, `/agentWorkspaces` goes into `.gitignore`, staged and not
    committed: tell the user to commit it.
24. When a dossier operation or a hook says "the dossiers are not migrated yet: the user runs legionctl dossier migrate", tell
    the user and go on without dossier writes; the hooks do not block you. The migration moves every dossier
    (class workspace): run it only when the user says so, `legionctl dossier migrate --dry-run` first, then
    `legionctl dossier migrate`. Your session keeps its dossier after the move.
25. A delegate reads the plan and the MEMORY and DESIGN that its rows name itself; the lead's prompt is a start, not
    the whole context (skill sessions-and-agents).

## Refs

- `refs/reference-hooks.md` - read to know what each plugin hook does, or to change hooks and reminders.
