---
description: Manage the reminders legionctl's hooks inject into every session or prompt (guided)
argument-hint: [on|off|add|remove] [id] [text]
---
Help the user manage legionctl reminders. Request: $ARGUMENTS

1. Call `reminders_list` and show a compact table: on/off, id, when (session start or every prompt), built-in or
   custom, text.
2. If the request already says what to do (for example `off ste100`, or `add tests-first "Write a test first"`),
   do it with `reminders_on`, `reminders_off`, `reminders_add` or
   `reminders_rm` and show the updated row.
3. Otherwise guide the user:
   - Offer the built-ins that are off, each with one line on what it does.
   - Offer to add a custom reminder. Ask for: a short id (a-z, 0-9, '-'), the text (one line, 300 characters max),
     and when it fires. Recommend "session start" unless it must apply on every turn, because every-prompt
     reminders cost tokens on each turn.
4. Confirm the change and say it applies from the next prompt or session start.
