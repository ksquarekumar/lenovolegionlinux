---
description: Set up ctx and legionctl for Claude Code and Codex (plugins, skills, hooks and MCP servers)
---
1. Run `setup.check` with the legionctl MCP tool `legionctl_run` and show what is missing. The rows
   `legionctl-system` (agent `machine`) and `legionctl-operator` (agent `user`) are the services: the package turns
   them on, and the user can turn them off. A row that is not `current` shows its state and, as its path, the line that turns it on.
2. If something is missing, tell the user what `setup.apply` changes: the ctx skill and MCP server for Claude Code and
   Codex, the legionctl plugin of Codex from the package folder (its MCP server, hooks and skills; an old
   `[mcp_servers.legionctl]` of ~/.codex/config.toml and the old skill links in ~/.codex/skills go, with a backup of
   the config), each missing ACP adapter of config `session.acp` (`bun add -g` with its package from
   `session.acp_packages`, into ~/.bun; bun runs them), and ~/.config/environment.d/99-legionctl.conf (~/.local/bin,
   ~/.cargo/bin and bun's global folder ~/.bun/bin before the PATH of the systemd user manager, so the operator's
   checks find the user's tools after the next login; headless runs get these folders at once), the legionctl plugin
   of Claude Code from the package folder, the sandbox access of the agents at user level, so that no repository
   commits it (in ~/.claude/settings.json, with one backup of the file: `sandbox.excludedCommands` gets `legionctl *`,
   `permissions.allow` gets `legionctl dossier` and `status`, `sandbox.filesystem` gets `~/.gitconfig` and
   legionctl's folders (`dossier.root`, `workspace.root`, its data, state and cache to write, its config to read);
   ~/.codex/rules/legionctl.rules lets Codex run `legionctl dossier` and `status` outside its sandbox with no prompt
   and asks the user before each other legionctl command; ~/.codex/config.toml gets the same folders in the custom
   permission profile that the user or a repository with legionctl workspaces names. Only the user's own layer for one
   repository (`legionctl config set <key> true --scope user-repo` in that repository) adds more:
   `workspace.claude_allows_git_rm` allows git -C, rm and rmdir in that repository's workspaces with no prompt (every
   git subcommand, git push too), `workspace.codex_writes_git` lets Codex's sandbox write that repository's `.git`; a
   repository's `.legionctl` cannot set them, and setup removes them for a repository that does not turn them on
   (the row says outdated until then)), the OSD extension of
   GNOME Shell, and clauth (the account engine: `cargo install clauth --version ~0.17 --locked` when missing, its
   daemon as the user unit `legionctl-clauth.service`, its MCP server in Claude Code). Then run `setup.apply`.
3. If ctx is not installed, tell the user to run `legionctl setup apply --install-ctx` in a terminal.
4. If only a service is off, run `setup.turn-on` with that service (`legionctl-system` or `legionctl-operator`). It
   turns on only that service, also when ctx is missing; polkit asks the user for `legionctl-system`. `setup.apply`
   also turns on each service that is off, first.
5. If the row `legionctl hooks` of Codex is not trusted yet, tell the user to start Codex once and choose to trust the
   legionctl hooks when Codex asks. Codex runs no hook before that, and only the user can give that trust.
