---
description: Set up ctx and legionctl for Claude Code and Codex (skills and MCP servers)
---
1. Call the legionctl MCP tool `setup_check` and show what is missing.
2. If something is missing, tell the user what `setup_apply` changes: the ctx skill and MCP server for Claude Code and
   Codex, legionctl's MCP server in ~/.codex/config.toml (with a backup), and legionctl's skill folders linked into
   ~/.codex/skills. Then call `setup_apply`.
3. If ctx is not installed, tell the user to run `legionctl setup apply --install-ctx` in a terminal.
