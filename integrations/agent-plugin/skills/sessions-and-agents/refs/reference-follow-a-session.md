# Follow a session

1. `legionctl session list`: the id and the state of its scope, container or VM.
2. `session run` writes the answer while it runs; `legionctl trace` in another terminal shows the `session.output`
   events. `--json` gives the result only at the end.
3. `legionctl jobs list`, then `legionctl jobs show <id>`: the result, or why it failed.
4. `legionctl session activity <id>`: the programs it started (eBPF sensor of `legionctl-system`).
5. `legionctl tools calls --session <id>`: the MCP calls it made.
6. `legionctl workspace status --topic <t>` or `workspace list`: its commits and changes.
7. `legionctl dossier nudges --of <id>`: whether it kept its dossier current.
8. `legionctl gateway usage` (each call through the gateway) and `legionctl agents usage [--days <n>]`
   (tokens from the agents' own transcripts): tokens and cost.
