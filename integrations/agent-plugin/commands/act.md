---
description: Request a machine change (power policy, desktop GPU, cleanup) that a human approves
argument-hint: power auto|ac|battery | gpu nvidia|intel|auto | cleanup
---
Request this change with the legionctl MCP tool that matches it: $ARGUMENTS

- power: `power_apply` with `source`
- gpu: `gpu_primary` with `choice`
- cleanup: `cleanup` (with `unload_modules` only when the user asks)

Pass `dry_run: true` first when the user wants to see the plan.

Tell the user first that a polkit approval dialog will appear on this machine and that nothing changes unless they
approve it. Afterwards report legionctl's output; for the desktop GPU, remind them it applies at the next login.
