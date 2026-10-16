# The test gate

`workspace checkpoint` reviews each new or changed test before it commits. A test is a function with an attribute
whose path ends in `test` (`#[test]`, `#[tokio::test]`, `#[sqlx::test]`, `#[rstest]`) or `#[test_case(..)]`; the
review parses the file, and the change set touches the test when a changed line is in its span. It refuses the checkpoint, and commits nothing, when a test has a hard signal:

| Signal | Rule |
| --- | --- |
| No assertion | No `assert!`, `assert_eq!`, `assert_ne!`, `assert_matches!`, `debug_assert*!` call, no `.unwrap_err()` call in the code (comments and strings do not count), and no `#[should_panic]`. |
| No kill | No mutant record (tool or agent) names this version of the test as a test that failed. |

A kill and an accept are for one version of a test: its file, its libtest name and the hash of its tokens. When the
test changes (not its comments or its place), or a test of the same name is in another file, the gate asks again.

Notes do not refuse: a duplicate (another test kills each mutant this test kills) and a test outside `tests/`.

## Evidence

1. Hand mutation: break the code, run the test, see it fail, put the code back. Then
   `legionctl code tests mutant record <file> <line> "<what the mutation does>" <test> [<test>...]`. With no test,
   the record says that the mutant survived. A `<test>` is a function name, a libtest name, or `<file>::<name>` when
   the name is in more than one file; the record takes the test as it is in the working tree.
2. Tool: with cargo-mutants installed (`cargo install cargo-mutants --locked`; CI does not have it),
   `legionctl code tests mutant run` mutates the Rust source files that the branch changed and records each mutant
   with the tests that killed it. It is a job (`jobs show <id>` shows the progress) and takes one suite run per
   mutant: minutes.

## When the gate refuses

1. Give the evidence, and run the checkpoint again.
2. Or let a test through when it has value that no mutant shows:
   `legionctl workspace checkpoint "<message>" --accept-test <test> --reason "<why>"`. Repeat `--accept-test` for
   more tests, with one `--reason` for all or one per test in the same order; all are accepted or none. The reason is
   kept with the test (`code tests show <test>`) and is an event (`code.tests.accept`). An accept stays for later checkpoints while
   the test does not change.

## Commands

| Command | What it shows |
| --- | --- |
| `code tests review [--staged [--index <file>]]` (`--index`: the client's `GIT_INDEX_FILE` when not given) | The new and changed tests against HEAD (the working tree, or the index): assertions, kills, duplicates, refusals. It fails when the gate refuses a test. |
| `code tests run [--filter <text>]` | Runs each `test` check of `.legionctl/checks.jsonc` as a job (the filter goes after `--`). For a check with `"runner": {"format": "libtest"}` (and cargo's `--message-format=json`) it records each test's binary source (relative to the repository), result and ref. When the check fails and no test ran, the job fails with the compiler's errors. |
| `code tests list`, `code tests show <test>` | The tests with their last result and kills; the runs, kills and accepts of one test. |

The pre-commit hook `legionctl-test-gate` (`scripts/test-gate.sh`) runs `code tests review --staged` for commits made
with plain `git commit`. `git commit -a` and `git commit <path>` commit an index file of their own: the client sends
the hook's `GIT_INDEX_FILE` as `--index`, and the review reads that index. With nothing staged (`make lint`, CI) it has nothing to review. When the `legionctl` on
`PATH` has no `code tests review`, it refuses the commit: install the new package (`make install-package`).
