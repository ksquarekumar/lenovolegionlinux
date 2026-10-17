---
description: Set up ctx and legionctl for Claude Code and Codex (skills and MCP servers)
---
1. Call the legionctl MCP tool `setup_check` and show what is missing.
2. If something is missing, tell the user what `setup_apply` changes: the ctx skill and MCP server for Claude Code and
   Codex, legionctl's MCP server in ~/.codex/config.toml (with a backup), and legionctl's skill folders linked into
   ~/.codex/skills, each missing ACP adapter of config `session.acp` (`bun add -g` with its package from
   `session.acp_packages`), and ~/.config/environment.d/99-legionctl.conf (~/.local/bin, ~/.cargo/bin and bun's
   global folder before the PATH of the systemd user manager, so the operator's checks and headless runs find the
   user's tools after the next login), the legionctl plugin of Claude Code from the package folder, and the OSD
   extension of GNOME Shell. Then call `setup_apply`.
3. If ctx is not installed, tell the user to run `legionctl setup apply --install-ctx` in a terminal.
