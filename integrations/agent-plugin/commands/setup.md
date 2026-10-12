---
description: Set up ctx and legion for Claude Code and Codex (skills and MCP servers)
---
Call the legion MCP tool `setup_check` and show what is missing. If anything is missing, tell the user what
`setup_apply` will change (ctx skill and MCP server for Claude Code and Codex, legion's MCP server in
~/.codex/config.toml with a backup, legion's skills linked into ~/.codex/skills), then call `setup_apply`.
If ctx itself is not installed, tell the user to run `legion setup --install-ctx` in a terminal.
