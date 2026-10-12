---
description: Set up ctx and legionctl for Claude Code and Codex (skills and MCP servers)
---
Call the legionctl MCP tool `setup_check` and show what is missing. If anything is missing, tell the user what
`setup_apply` will change (ctx skill and MCP server for Claude Code and Codex, legionctl's MCP server in
~/.codex/config.toml with a backup, legionctl's skills linked into ~/.codex/skills), then call `setup_apply`.
If ctx itself is not installed, tell the user to run `legionctl setup apply --install-ctx` in a terminal.
