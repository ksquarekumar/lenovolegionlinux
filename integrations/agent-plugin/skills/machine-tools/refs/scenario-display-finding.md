# Scenario: a display finding

1. `legionctl display status` shows `[refresh-DP-3]`.
2. `legionctl display fix refresh-DP-3 --dry-run` shows the new and the old value.
3. With the user's agreement: `legionctl display fix refresh-DP-3`.
4. To turn it back: find its `display.change` event with `legionctl events`, then `legionctl undo <event>`.
