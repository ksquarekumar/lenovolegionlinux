---
description: Request a machine change (power policy or desktop GPU) that a human approves
argument-hint: apply-power-policy auto|ac|battery | set-desktop-gpu nvidia|intel|auto
---
Request this change with the legionctl MCP tool `request_action`: $ARGUMENTS

Tell the user first that a polkit approval dialog will appear on this machine and that nothing changes unless they
approve it. Afterwards report legionctl's output; for set-desktop-gpu, remind them it applies at the next login.
