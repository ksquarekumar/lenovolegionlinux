# Reference: agent configs and teams

An agent is three layers, lowest first. `agents check` shows the layer that set each field (`set_by`).

| Layer | Where | May |
| --- | --- | --- |
| archetype file | `integrations/agent-plugin/agents/<archetype>.md` | set the instruction text, `effort`, `skills`, `tools` |
| config | `agents.configs.<name>` in the config layers | change `account`, `model`, `effort`, `pace`, `budget`; add `skills`, `mcp`, `instructions`; narrow `tools`, `commands` |
| team member | `teams/<team>.jsonc` (`.legionctl/teams/`, `~/.config/legionctl/teams/`, the plugin) | narrow `tools`, `commands`, `budget`; set `pace`, `model`; `flow.may` removes message kinds |

The repository config layer may only narrow too. The shipped team is `vanilla`: `main0`, `observer0`, `ui0`.

Each member runs on one run path. A field that its path cannot apply refuses the member:

| Run path | When | Refused fields (set by a config or a team) |
| --- | --- | --- |
| `claude-session` | the main agent with Claude Code | `sandbox`, `approval` |
| `claude-team` | a member with a Claude Code lead (a teammate; a plugin agent type) | all but `model`, `pace`, `budget` |
| `codex-session` | the main agent with Codex | `tools` |
| `codex-subagent` | a member with a Codex lead (`~/.codex/agents/<config>.toml`) | `tools`, `mcp`, `account`, `commands`, `sandbox`, `approval`; from a team also `model`, `effort`, `skills`, `instructions` |

`setup apply` writes each config that is not `"harness": "claude"` as a Codex agent: name, description, the
instruction text, `model` and `model_reasoning_effort` only.

```jsonc
// ~/.config/legionctl/agents.jsonc
{ "agents": { "configs": {
  "thread-observer": { "model": "sonnet", "budget": { "tokens_per_day": 1000000 } },
  "strict-main": { "archetype": "main-agent", "commands": { "deny+": ["Bash(cargo publish*)"] } }
} } }
```
