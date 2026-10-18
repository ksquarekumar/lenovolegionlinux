# Example: observer0 blocks a weak test

Trigger: main0 edits `crates/legionctl/tests/threads.rs` and runs `workspace checkpoint` soon after.

1. observer0 runs `legionctl code tests review`. The new test `peers_are_listed` has one assertion,
   `assert!(!peers.is_empty())`, and no kill.
2. The test passes when the operator lists the wrong peers. The fault makes the result of main0 wrong.
3. observer0 sends:

```text
legionctl thread send main0 hard-block --tool workspace.checkpoint "peers_are_listed has no kill and asserts only
a non-empty list. Assert the peer's session and state, record a mutation (code tests mutant record), then ack."
```

4. When the test only reads badly but has a kill, observer0 sends a `hint` instead.
