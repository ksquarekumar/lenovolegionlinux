---
description: Bind this session to a dossier (legionctl:dossier), create one, or show and release the binding
argument-hint: use <name> [--create] [--steal] | active | release | ls | check
---
legionctl:dossier request: $ARGUMENTS

Use the legionctl MCP tools; the session id is filled in for you.

- `use <name>`: call `dossier_use` with `name` (and `create: true` for `--create`, `steal: true` for `--steal`). A
  dossier held by another live session is refused: show who holds it and ask the user before you pass `steal`.
  Then read `<name>/plan.md` and the newest MEMORY files with `dossier_read`, and follow the
  `legionctl:dossier-practice` skill for the rest of the session.
- `active`: call `dossier_active`. `release`: call `dossier_release`. `ls`: call `dossier_ls` and show the names.
- `check`: call `dossier_check` and list the findings.
- No arguments: call `dossier_active` and `dossier_ls`, then ask which dossier to use or what name to create.

Until a dossier is bound, legionctl blocks every other prompt of this session.
