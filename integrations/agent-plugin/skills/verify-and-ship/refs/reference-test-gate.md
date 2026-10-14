# The test gate

`workspace checkpoint` reviews each new or changed test (a `#[test]` function that the change set touches) before it
commits. It refuses the checkpoint, and commits nothing, when a test has a hard signal:

| Signal | Rule |
| --- | --- |
| No assertion | No `assert*!`, `panic!`, `unwrap_err`, `expect_err` in the body. |
| No kill | No mutant record (tool or agent) names the test as a test that failed. |

Notes do not refuse: a duplicate (another test kills each mutant this test kills) and a test outside `tests/`.

## Evidence

1. Hand mutation: break the code, run the test, see it fail, put the code back. Then
   `legionctl code tests mutant record <file> <line> "<what the mutation does>" <test> [<test>...]`. With no test,
   the record says that the mutant survived.
2. Tool: with cargo-mutants installed (`cargo install cargo-mutants --locked`; CI does not have it),
   `legionctl code tests mutant run` mutates the Rust source files that the branch changed and records each mutant
   with the tests that killed it. It is a job (`jobs show <id>` shows the progress) and takes one suite run per
   mutant: minutes.

## When the gate refuses

1. Give the evidence, and run the checkpoint again.
2. Or let one test through when it has value that no mutant shows:
   `legionctl workspace checkpoint "<message>" --accept-test <test> --reason "<why>"`. The reason is kept with the
   test (`code tests show <test>`) and is an event (`code.tests.accept`). An accept stays for later checkpoints.

## Commands

| Command | What it shows |
| --- | --- |
| `code tests review [--staged]` | The new and changed tests against HEAD (the working tree, or the index): assertions, kills, duplicates, refusals. It fails when the gate refuses a test. |
| `code tests run [--filter <text>]` | Runs `cargo test --release --workspace --no-fail-fast -j8` as a job and records each test's file, result and ref. |
| `code tests list`, `code tests show <test>` | The tests with their last result and kills; the runs, kills and accepts of one test. |

The pre-commit hook `legionctl-test-gate` (`scripts/test-gate.sh`) runs `code tests review --staged` for commits made
with plain `git commit`. With nothing staged (`make lint`, CI) it has nothing to review. When the `legionctl` on
`PATH` has no `code tests review`, it refuses the commit: install the new package (`make package install-package`).
