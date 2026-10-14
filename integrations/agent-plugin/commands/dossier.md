---
description: Bind this session to a dossier (legionctl:dossier), create one, or show and release the binding
argument-hint: use <name> [--steal] | active | release | list | check
---
legionctl:dossier request: $ARGUMENTS

Use the legionctl MCP tools; the client fills in the session id. Follow skill `legionctl:dossier-practice`.

1. `use <name>`: call `dossier_use` with `name`. If it is refused, show who holds the dossier and ask before you pass
   `steal: true`.
2. After a bind, read `<name>/plan.md` and the newest MEMORY files with `dossier_read`.
3. `active`, `release`, `list`, `check`: call `dossier_active`, `dossier_release`, `dossier_list` or `dossier_check` and
   show the result.
4. No arguments: call `dossier_active` and `dossier_list`, then ask which dossier to use or what name to create.
