---
name: verify-and-ship
description: The checks before a legionctl change is committed and merged - release build, static client, lint, tests, mutation checks, regenerated schema and D-Bus XML, EVIDENCE, commit message, and CI in batches. Use before each checkpoint that changes code, before a merge, and when CI fails.
user-invocable: true
---

# Verify and ship

1. A change is complete when the checks pass in the workspace, each new test caught a broken version of the code, the
   dossier has the EVIDENCE, and the commit message says what was checked.
2. Run each check in the workspace path (skill workspace-practice), never in the main checkout.
3. Build first: `cargo build --release --workspace --bins --locked`. `cargo test -p legionctl` does not build the
   operator and daemon again, so without this the tests run old binaries.
4. Run `make static` before the container tests. They put `target/static/release/legionctl` into the agent image.
5. Run `make lint`, then `make rust-lint` (and `make js-lint` when JS changed, `make validate` when a profile or layer
   changed).
6. Do not suppress a lint (`#[allow]`, `noqa`, a skipped hook). If a rule fights the design, tell the user which rule,
   where and why, and let the user decide.
7. Run the tests with `cargo test --release --workspace --locked --no-fail-fast` when the machine is not busy (skill
   tool-lifecycle, load).
8. Tests are end to end, from a boundary that a user or agent uses, with high diagnostic value. Do not write a test that
   checks the obvious.
9. The `#[ignore]` tests (root eBPF and VM tests) run only in their own CI steps (`.github/workflows/ci.yml`). Do not
   run the root tests on this machine; say in the commit message that CI runs them.
10. Mutation-check each new test: break the behavior it guards with one small change, run only that test, see it fail,
    put the code back, see it pass. Do this for each behavior the test claims.
11. When an operation, its class or a wire type changes, write `docs/api/schema.json` again: `LEGIONCTL_WRITE_SCHEMA=1
    cargo test -p legionctl --test e2e the_wire_format`.
12. When a method or property of `legionctl-system` changes, write `dbus/org.legionctl.System1.xml` again:
    `LEGIONCTL_WRITE_DBUS=1 cargo test -p legionctl --test machine the_daemon_serves`.
13. Read the diff of each generated file before you commit it. A change that you did not intend is a bug.
14. Write the EVIDENCE from the real output when the check runs: `dossier add EVIDENCE/validation <slug>` with the
    commands, the counts (passed, failed, ignored), the mutations and the commit SHA, and front matter `id`, `class`,
    `status`, `code`, `supports` (skill dossier-practice).
15. Write the commit message as one long paragraph: what changed and why, the decisions, the tests by name, each
    mutation (and each survivor and why it is equivalent), and what is not tested and why. `git log -5` shows the style.
16. End the message with the attribution line that the session gives (`Co-Authored-By: ...`). Commit with `workspace
    checkpoint` (skill workspace-practice).
17. The user pays for CI minutes. Commit locally; push only when the user or the lead says so, with a larger tested
    batch.
18. CI is not the first test run. Run the local checks before each push.
19. Follow a run with `gh run list` and `gh run view <run-id>`; read a failure with `gh run view <run-id> --log-failed`.
20. A failed step stops the steps after it. When `make ci` fails, the eBPF and VM steps did not run; do not report them
    as passed.
21. Fix the cause locally, run the local checks again, and push the fix with the next batch.

## Refs

- `refs/reference-make-targets.md` - read to know what each make target runs.
- `refs/scenario-new-cli-operation.md` - read when you add a CLI operation.
