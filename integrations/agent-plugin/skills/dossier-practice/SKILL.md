---
name: dossier-practice
description: How to keep this session's legionctl dossier (~/.agentworkspace/<dossier>) - the ledger in plan.md, numbered MEMORY, DESIGN, EVIDENCE and SPIKE artifacts, routing rules, anchored edits. Use whenever a session has a dossier bound (/legionctl:dossier).
user-invocable: false
---

# Dossier practice

A dossier is your own space for one scope, outside the product's git tree: `~/.agentworkspace/<dossier>/`. Your
session writes only to its active dossier (`/legionctl:dossier use <name>`); you can read every dossier.

## Artifacts

| Class | Path | Holds |
| --- | --- | --- |
| PLAN | `plan.md` | The ledger: state only, short entries that point to the artifact with the detail |
| MEMORY | `MEMORY/NN-<topic>.md` | Intake notes, source reads, decision and ambiguity registers, context moved from chat |
| DESIGN | `DESIGN/...` | Intended design, separate from plan status and from evidence |
| EVIDENCE | `EVIDENCE/<class>/.../NN-<scope>.md` | Validation logs, reports, inventories, comparisons, from real output |
| SPIKE | `SPIKE/...` | Small, disposable prototypes and experiments |

Create a numbered artifact with `dossier_add` (class, slug, content); legionctl picks the next number.

## Routing rules

1. No plan content yet: write a MEMORY document first.
2. `plan.md` records state. An entry that grows past a few sentences moves to MEMORY or EVIDENCE, with a pointer.
3. Context from the chat that later work depends on goes into MEMORY in the same turn.
4. Design content lives in DESIGN; the plan's `Design` section points to it.
5. Validation results live in EVIDENCE; the plan summarizes their state.
6. Reference, do not copy: point to artifacts and repository files by path.

## Ledger rules

- Keep requested, proposed, approved, implemented and validated state distinct. Never describe a proposal as a
  decision, or a change as validated without EVIDENCE.
- Update the dossier in the same turn as the finding or decision.
- Keep every section of `plan.md`; `dossier_check` reports a missing one.

## Editing

`dossier_read` prints each line as `<line>:<hash>|<text>`. `dossier_edit` takes those anchors (`replace`,
`insert-after`, `delete`); an anchor from an outdated read is refused, so read again and retry. Use `dossier_write`
for a new file and `dossier_append` to add to one.
