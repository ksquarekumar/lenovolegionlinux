# Scenario: one agent, one change

1. `legionctl dossier use legionctl`
2. `legionctl -C /data/git/lenovolegionlinux workspace open`
3. Edit files in the returned path. Run the checks (skill verify-and-ship).
4. `legionctl workspace checkpoint "Kernels: list, default and try through the boot takeover"`
5. Write the SHA into the `plan.md` row with `dossier edit`.
6. Ask the user. Then `legionctl workspace merge`.
7. `legionctl workspace close` when the work in this repository is complete.
