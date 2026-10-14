# Scenario: the user wants a lower CPU power limit on AC

1. Read: `legionctl knobs show firmware.ppt_pl1_spl` and `legionctl plan`.
2. Propose the layer: `legionctl rules edit 50-quiet firmware.ppt_pl1_spl=90 --when power=ac`. Set the knob for battery
   too, or the check refuses the layer.
3. Give the user the command. The user runs it.
4. Check: `legionctl plan`, `legionctl knobs show firmware.ppt_pl1_spl`, `legionctl doctor`.
5. To turn it back: `legionctl rules edit 50-quiet firmware.ppt_pl1_spl=`, or `legionctl rules reset`.
