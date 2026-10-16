# Plugin hooks

The plugin's `hooks/hooks.json` runs `legionctl hook <event>` in each Claude Code session. `hooks list` shows the
actions bound to each event; `/legionctl:hooks` and `/legionctl:reminders` change them.

| Event | What it does |
| --- | --- |
| `session-start` | Binds the session to the last dossier used in this place, when it is free (`dossier.auto_bind`). Tells you the dossier and the workspace (with drift), or tells you to open one. Runs the bound actions (default: reminders, load, setup-check). |
| `prompt` | Blocks each prompt while the session has no dossier (`dossier.gate`); only `/legionctl:dossier` passes. Runs the bound actions (default: reminders, load-if-busy). Gives the new thread messages to you (note, hint, reminder, notice, result) as one block, once. |
| `pre-tool` | Runs for each tool. Denies a tool that an open thread hard-block names, and `Write`, `Edit`, `MultiEdit` and `NotebookEdit` into a dossier that is not this session's. |
| `subagent-start` | Runs when the agent starts a subagent. In a session with a dossier, the subagent gets the delegation rule in its context: read the dossier plan and the MEMORY and DESIGN files its rows name, review adversarially, keep messages short and link the dossier. |
| `post-tool` | Runs for each tool. Records the call (presence, the watch rules of your thread), and each `SendMessage` in the thread log. |
| `stop` | An open thread hard-block stops each reply end; a soft-block stops one. The dossier nudge (`dossier.nudge_turns`). It also tells you when the main checkout has changes and the dossier has no workspace there (`workspace.nudge`). |
| `session-end` | Releases the dossier binding. |
