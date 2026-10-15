# Example: observer0 backfills a dossier record

Trigger: main0 made checkpoint `c48fa60` and wrote no ledger row for it.

1. `legionctl thread peers` shows main0 `idle`. If main0 is `running`, wait for the next idle window.
2. Read the ledger: `legionctl dossier read <main dossier>/plan.md`. Find the end of the ledger table.
3. Append one row, marked as yours:

```text
legionctl dossier append <main dossier>/plan.md --content "| done-41 | checkpoint c48fa60, lint and suite clean | (by observer0) |"
```

4. Do not change a row of main0. Do not edit, move or delete a line.
5. If the operator refuses the write (main0 holds the lock, or it changed the file), send a `reminder` to main0
   instead.
6. Send one notice: `thread send main0 notice "observer0 wrote for you: plan.md row done-41 (checkpoint c48fa60)"`.
