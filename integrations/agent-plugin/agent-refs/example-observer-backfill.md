# Example: observer0 backfills a dossier record

Trigger: main0 made checkpoint `c48fa60` and wrote no ledger row for it.

1. Read the ledger: `legionctl dossier read <main dossier>/plan.md`. Find the end of the ledger table.
2. Append one line as a member. The operator adds the mark `(by observer0)`:

```text
legionctl dossier append --member observer0 <main dossier>/plan.md --content "| done-41 | checkpoint c48fa60, lint and suite clean |"
```

3. Do not change a line of main0. Do not edit, move or delete a line.
4. If the operator refuses the write (main0 is `running`), send a `reminder` to main0 instead:
   `thread send main0 reminder "record checkpoint c48fa60 in plan.md"`.
5. The operator sends main0 one `notice` with the line and the command that undoes it (`thread compensate <id>`).
