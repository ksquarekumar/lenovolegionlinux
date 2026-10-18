---
description: Bind this session to a dossier (legionctl:dossier), create one, or show and release the binding
argument-hint: use <name> [--steal] | active | release | list | check | rename <name> <to>
---
legionctl:dossier request: $ARGUMENTS

Run each operation with the legionctl MCP tool `legionctl_run` (`op`, `input`); the client fills in the session id.
Follow skill `legionctl:dossier-practice`.

1. `use <name>`: run `dossier.use` with `name`. If it is refused, show who holds the dossier and ask before you pass
   `steal: true`.
2. After a bind, read `<name>/plan.md` and the newest MEMORY files with `dossier.read`.
3. `active`, `release`, `list`, `check`: run `dossier.active`, `dossier.release`, `dossier.list` or `dossier.check` and
   show the result.
4. `rename <name> <to>`: run `dossier.rename`; the operator asks the user. A name in use is refused.
5. No arguments: run `dossier.active` and `dossier.list`, then ask which dossier to use or what name to create.
