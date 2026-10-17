---
description: Manage what legionctl's agent hooks run at session start and on every prompt (guided)
argument-hint: [bind|unbind <event> <action>] [add <id> "<command>"] [rm <id>]
---
Help the user manage legionctl hook bindings. Request: $ARGUMENTS

1. Call `hooks_list`. Show the bindings of each event in run order, then the actions (built-in with one line each; the
   user's own with their command and timeout).
2. If the request says what to do, call `hooks_bind`, `hooks_unbind`, `hooks_add` or `hooks_rm`, and show the new
   bindings. The change asks the user on the desktop first (Allow or Deny); tell the user to answer it.
3. Else offer the built-in actions that are not bound. Each action bound to `prompt` runs on each turn and costs time
   and tokens.
4. For a new action, ask for an id (a-z, 0-9, '-'), a one-line shell command whose output the agent sees, a timeout
   (1-30 s, default 5) and the event. The command runs as the user on each event.
5. For reminder text (not a command), use `/legionctl:reminders`.
6. Say that the change applies from the next prompt or session start.
