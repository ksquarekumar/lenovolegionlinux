---
description: Manage what legion's agent hooks run at session start and on every prompt (guided)
argument-hint: [bind|unbind <event> <action>] [add <id> "<command>"] [remove <id>]
---
Help the user manage legion hook bindings. Request: $ARGUMENTS

1. Call `hooks_list`. Show the bindings per event in run order, then the available actions (built-in with a
   one-line description; user-defined with their command and timeout).
2. If the request already says what to do, do it with `hooks_update` and show the new bindings.
3. Otherwise guide the user:
   - Offer built-in actions that are not bound yet, and explain the cost: every action bound to "prompt" runs on
     every turn, so it adds delay and tokens.
   - To add their own action, ask for a short id (a-z, 0-9, '-'), a one-line shell command whose output the agent
     should see, a timeout (1-30 s, default 5) and which event to bind it to. Warn that the command runs as the user
     on every matching event.
   - For reminder text (not a command), send them to `/legion:reminders` instead.
4. Confirm the change; it applies from the next prompt or session start.
