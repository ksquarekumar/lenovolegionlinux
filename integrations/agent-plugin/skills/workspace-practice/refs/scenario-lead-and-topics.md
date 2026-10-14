# Scenario: a lead and two parallel agents

1. The lead binds the dossier and gives each agent a topic: `kernels` and `display`.
2. Each agent runs `legionctl workspace open --topic kernels` (or `--topic display`) and works in its own path.
3. Each agent commits from inside its path: `legionctl workspace checkpoint "<message>"`. With MCP tools, each call gets
   `topic`.
4. The lead merges one topic at a time: `legionctl workspace merge --topic kernels`, then `--topic display`.
5. On a conflict, the lead follows the conflict rules of the skill, then merges again.
6. The lead closes each topic: `legionctl workspace close --topic kernels`. The branch stays.
