---
description: Manage the reminders legionctl's hooks inject into every session or prompt (guided)
argument-hint: [on|off|add|rm] [id] [text]
---
Help the user manage legionctl reminders. Request: $ARGUMENTS

1. Call `reminders_list`. Show a compact table: on/off, id, when (session start or each prompt), built-in or custom,
   text.
2. If the request says what to do (`off ste100`, `add tests-first "Write a test first"`), call `reminders_on`,
   `reminders_off`, `reminders_add` or `reminders_rm`, and show the new row. The change asks the user on the desktop
   first (Allow or Deny); tell the user to answer it.
3. Else offer the built-ins that are off, with one line each, and offer a custom reminder.
4. For a custom reminder, ask for an id (a-z, 0-9, '-'), the text (one line, 300 characters max) and when it fires.
   Recommend session start: a prompt reminder costs tokens on each turn.
5. Say that the change applies from the next prompt or session start.
