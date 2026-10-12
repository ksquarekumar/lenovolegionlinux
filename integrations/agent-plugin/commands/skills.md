---
description: Show legionctl skills, load one now, or turn its reminder on or off
argument-hint: [skill] [load|on|off]
---
Manage legionctl skills. Request: $ARGUMENTS

1. Call `skills`. Show one row per skill: name (`legionctl:<name>`), one-line purpose, installed for Claude Code
   and Codex (yes/no), and reminder on/off.
2. Then act on the request:
   - `<skill>` or `<skill> load`: load and follow the skill `legionctl:<skill>` for the rest of this session.
   - `<skill> on|off`: call `reminders_on` or `reminders_off` with the skill's reminder id from step 1, so
     the hooks keep (or stop) reminding every session.
   - No arguments: after the table, ask which skill to load or which reminder to change.
3. If a skill is missing for Codex, suggest `/legionctl:setup`. To add your own reminders, point to `/legionctl:reminders`.
