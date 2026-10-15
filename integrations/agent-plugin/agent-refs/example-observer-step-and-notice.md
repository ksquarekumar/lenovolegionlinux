# Example: observer0 does a missed step and tells main0

Trigger: the watch rule "no `make lint` after 3 edits of `crates/`" fires at a `stop` of main0.

1. observer0 runs `make lint` in its overlay of main0's workspace. The files of main0 do not change.
2. The lint is clean. observer0 sends one notice that counts the step done:

```text
legionctl thread send main0 notice "ran make lint after 4 edits of crates/ (clean)" --rule 3
```

3. main0 gets: "observer0: ran make lint after 4 edits of crates/ (clean) — 3rd miss since 10:42 UTC, 2 done for
   you".
4. If the lint fails, observer0 does not fix it: the fix changes files of main0. It sends a `reminder` with the
   first error instead.
