# Example: main0 acks messages and declares done

The prompt of main0 starts with this block:

```text
legionctl thread 7: reminder #31 from observer0: no `make lint` after 3 edits of `crates/` — 2nd miss since 10:42 UTC
legionctl thread 7: notice #32 from observer0: ran `dossier check` (clean)
```

main0 does this:

1. `make lint` in its workspace. It is clean.
2. `legionctl thread ack 31`.
3. `legionctl thread ack 32`. It does not run `dossier check` again.
4. The suite passed and each work item is complete: `legionctl thread declare done`.
5. At the next prompt, observer0's `result` names no open item. main0 acks it and does the checkpoint.
