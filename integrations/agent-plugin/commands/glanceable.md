---
description: Load the glanceable-output skill now and manage its reminder
argument-hint: [on|off]
---
1. Load and follow the skill `legion:glanceable-style` for all user-facing output in this session.
2. Reminder for it ($ARGUMENTS): if the argument is `on` or `off`, call `reminders_update` with action `on`/`off`
   and id `glanceable`. Without an argument, call `reminders_list` and tell the user whether the `glanceable`
   reminder fires on every prompt, and that `/legion:glanceable off` stops it.
