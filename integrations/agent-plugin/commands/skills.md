---
description: Show legionctl skills, load one now, or turn its reminder on or off
argument-hint: [skill] [load|on|off]
---
Manage legionctl skills. Request: $ARGUMENTS

1. Call `skills`. Show one row for each skill: `legionctl:<name>`, its purpose in one line, installed for Claude Code
   and Codex (yes/no), reminder on/off.
2. `<skill>` or `<skill> load`: load skill `legionctl:<skill>` and follow it for the rest of this session.
3. `<skill> on|off`: call `reminders_on` or `reminders_off` with the skill's reminder id from step 1.
4. No arguments: after the table, ask which skill to load or which reminder to change.
5. If a skill is missing for Codex, suggest `/legionctl:setup`. For your own reminders, use `/legionctl:reminders`.
