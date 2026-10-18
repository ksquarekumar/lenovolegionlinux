---
description: Request a machine change (power policy, desktop GPU, cleanup) that a human approves
argument-hint: power auto|ac|battery | gpu nvidia|intel|auto | cleanup
---
Request this change with the legionctl operation that matches it (MCP tool `legionctl_run`): $ARGUMENTS

1. Follow skill `legionctl:machine-tools`. Tell the user that a polkit dialog will appear and nothing changes without
   their approval.
2. power: `power.apply` with `source`. gpu: `gpu.primary` with `choice`. cleanup: `cleanup` (`unload_modules` only when
   the user asks).
3. If the user wants to see the plan, pass `dry_run: true` first.
4. Report legionctl's output. For the desktop GPU, say that it applies at the next login.
