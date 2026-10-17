---
description: Set up ctx and legionctl for Claude Code and Codex (skills and MCP servers)
---
1. Call the legionctl MCP tool `setup_check` and show what is missing. The rows `legionctl-system` (agent `machine`)
   and `legionctl-operator` (agent `user`) are the services: the package turns them on, and the user can turn them
   off. A row that is not `current` shows its state and, as its path, the line that turns it on.
2. If something is missing, tell the user what `setup_apply` changes: the ctx skill and MCP server for Claude Code and
   Codex, legionctl's MCP server in ~/.codex/config.toml (with a backup), and legionctl's skill folders linked into
   ~/.codex/skills, each missing ACP adapter of config `session.acp` (`bun add -g` with its package from
   `session.acp_packages`, into ~/.bun; bun runs them), and ~/.config/environment.d/99-legionctl.conf (~/.local/bin,
   ~/.cargo/bin and bun's global folder ~/.bun/bin before the PATH of the systemd user manager, so the operator's
   checks find the user's tools after the next login; headless runs get these folders at once), the legionctl plugin
   of Claude Code from the package folder, the OSD extension of GNOME Shell, and clauth (the account engine: `cargo
   install clauth --version ~0.17 --locked` when missing, its daemon as the user unit `legionctl-clauth.service`, its
   MCP server in Claude Code). Then call `setup_apply`.
3. If ctx is not installed, tell the user to run `legionctl setup apply --install-ctx` in a terminal.
4. If only a service is off, call `setup_turn_on` with that service (`legionctl-system` or `legionctl-operator`). It
   turns on only that service, also when ctx is missing; polkit asks the user for `legionctl-system`. `setup_apply`
   also turns on each service that is off, first.
