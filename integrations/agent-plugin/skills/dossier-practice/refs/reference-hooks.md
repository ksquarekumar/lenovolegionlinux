# Plugin hooks

The plugin's `hooks/hooks.json` runs `legionctl hook <event>` in each Claude Code session. A hook blocks only by the
rule in its row. A failure (legionctl-operator is down, an event that this legionctl does not know) exits 1: Claude
Code shows it and goes on. `hooks list` shows the
actions bound to each event; `/legionctl:hooks` and `/legionctl:reminders` change them.

| Event | What it does |
| --- | --- |
| `session-start` | Binds the session to the last dossier used in this place, when it is free (`dossier.auto_bind`). Tells you the dossier and the workspace (with drift), or tells you to open one. Runs the bound actions (default: reminders, load, setup-check). |
| `prompt` | In a repository where a dossier was used, binds that dossier when it is free, else blocks each prompt while the session has no dossier (`dossier.gate`); only `/legionctl:dossier` passes. Elsewhere it never blocks. A run that a lead session started from its shell (Codex from Claude Code) and that has no dossier reads the lead's dossier and is not blocked; in Codex a blocked prompt arrives as an order to reply with only the gate's text. Runs the bound actions (default: reminders, load-if-busy). Gives the new thread messages to you (note, hint, reminder, notice, result) as one block, once. |
| `pre-tool` | Runs for each tool. Denies a tool that an open thread hard-block names, and `Write`, `Edit`, `MultiEdit` and `NotebookEdit` into a dossier: another session's, or this session's own (the dossier commands write it, with their checks and the status history). A write from Bash is not checked. |
| `subagent-start` | Runs when the agent starts a subagent. In a session with a dossier, the subagent gets the delegation rule in its context: read the dossier plan and the MEMORY and DESIGN files its rows name, review adversarially, keep messages short and link the dossier. |
| `pre-compact` | Runs before a compaction. In a session with a dossier, it writes an immutable HEALTH note (`dossier health`) and a JOURNAL entry with the session's status changes and checkpoints; the session start after the compaction points to both. |
| `post-tool` | Runs for each tool. Records the call (presence, the watch rules of your thread), and each `SendMessage` in the thread log. |
| `stop` | An open thread hard-block stops each reply end; a soft-block stops one. The dossier nudge (`dossier.nudge_turns`). It also tells you when the main checkout has changes and the dossier has no workspace there (`workspace.nudge`). |
| `session-end` | Releases the dossier binding. |
