# Scenario: main0 declares done and the verification fails

1. main0 runs `legionctl thread declare done`. observer0 wakes.
2. observer0 runs the suite in its overlay. One test fails: `threads::blocks_hold_main_until_acked`.
3. observer0 runs `legionctl code plan status --feature agent-network`. Work item P6a has one test `not run since
   change`.
4. observer0 sends one `result` that names each open item:
   "verification: 1 failed test (threads::blocks_hold_main_until_acked), P6a: 1 test not run since change".
5. The suite failed, so observer0 also sends a `hard-block` on `workspace_checkpoint` with the failed test.
6. main0 fixes the test, runs the suite, acks both messages and declares `done` again.
