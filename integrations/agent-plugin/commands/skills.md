---
description: Show legion skills, load one now, or turn its reminder on or off
argument-hint: [skill] [load|on|off]
---
Manage legion skills. Request: $ARGUMENTS

1. Call `skills_list`. Show one row per skill: name (`legion:<name>`), one-line purpose, installed for Claude Code
   and Codex (yes/no), and reminder on/off.
2. Then act on the request:
   - `<skill>` or `<skill> load`: load and follow the skill `legion:<skill>` for the rest of this session.
   - `<skill> on|off`: call `reminders_update` with action `on`/`off` and the skill's reminder id from step 1, so
     the hooks keep (or stop) reminding every session.
   - No arguments: after the table, ask which skill to load or which reminder to change.
3. If a skill is missing for Codex, suggest `/legion:setup`. To add your own reminders, point to `/legion:reminders`.
