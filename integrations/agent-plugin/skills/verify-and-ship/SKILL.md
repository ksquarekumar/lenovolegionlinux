---
name: verify-and-ship
description: The checks before a legionctl change is committed and merged - release build first, the static client, make lint, the full test suite, mutation checks of each new test, the regenerated API schema and D-Bus XML, EVIDENCE in the dossier, and CI in batches because CI minutes cost money. Use before each workspace checkpoint that changes code, before a merge, and when CI fails.
user-invocable: true
---

# Verify and ship

A change is complete when the checks pass in the workspace, each new test has caught a broken version of the code,
the dossier has the EVIDENCE, and the commit message says what was checked. Run every command in the workspace
path (skill `legionctl:workspace-practice`), never in the main checkout.

## What each check is and how it runs

| Check | What it does | How it runs |
| --- | --- | --- |
| `cargo build --release --workspace --bins --locked` | Builds `legionctl`, `legionctl-operator`, `legionctl-system` and the app. | Local, a few minutes. Run it first. |
| `make static` | Builds the static `legionctl` (musl) that `workspace pack` puts into agent images. | In the `rust:alpine` image with docker. |
| `make lint` | Every pre-commit hook on all files: whitespace, end of file, JSON, TOML, YAML, shell, rustfmt and clippy, Biome, profile validation, actionlint. | Local. |
| `make rust-lint` | `cargo fmt --check`, clippy pedantic with warnings as errors, `cargo machete`, `cargo deny check`. | Local. |
| `make js-lint` | Biome and tsc for the GNOME Shell extension. | Local; only when JS changed. |
| `make validate` | Each profile and its shipped configset layers. | Local, no hardware access. |
| `cargo test --release --workspace --locked --no-fail-fast` | The e2e and behavior tests. The CLI tests start the real operator and daemon binaries. | Local. Check `load` first and run it when the machine is not busy. |
| `make ci` | `lint`, `rust-lint`, `js-lint`, `validate` and `test`: the first CI step. | Local or CI. |

Rules that are not obvious:

1. Build the binaries before the tests, in the same profile. `cargo test -p legionctl` does not build the daemon and
   operator binaries again, so the tests run the old ones.
2. Run `make static` before the container tests. They put `target/static/release/legionctl` into the agent image.
3. Do not suppress a lint (`#[allow]`, `noqa`, a skipped hook). If a rule fights the design, tell the user which
   rule, where and why, and let the user decide.
4. Tests are end to end, from the boundary that a user or agent uses, with high diagnostic value. Do not write a
   test that checks the obvious.

## Tests that run only in CI

These tests are `#[ignore]`. CI runs them in their own steps (`.github/workflows/ci.yml`):

- the eBPF sensor tests in `crates/legionctl/tests/machine.rs` (`as_root_*`), with sudo;
- the VM tests in `crates/legionctl/tests/e2e.rs` (`sessions_in_a_vm_work_on_the_shared_workspace_behind_the_git_gate`,
  `the_legionctl_wmi_module_gives_the_legion_wmi_through_sysfs`), with KVM and QEMU.

Do not run the root tests on this machine. Say in the commit message that they run in CI.

## Mutation checks

Each new test must fail when the code it guards is wrong.

1. Make one small change that breaks the behavior (remove a check, change a value, skip a step).
2. Run only that test. It must fail.
3. Put the code back. Run the test again. It must pass.
4. Do this for each behavior the test claims to cover.
5. Name each mutation in the commit message ("Mutation-checked: no root= check, the line written before the
   image, ..."). Name each mutation that survived and why it is equivalent.

## Generated contracts

The wire format and the D-Bus interface are committed files. A test compares them with the code.

| File | When it changes | Write it again |
| --- | --- | --- |
| `docs/api/schema.json` | An operation, its class, or a type on the wire changed. | `LEGIONCTL_WRITE_SCHEMA=1 cargo test -p legionctl --test e2e the_wire_format` |
| `dbus/org.legionctl.System1.xml` | A method or property of `legionctl-system` changed. | `LEGIONCTL_WRITE_DBUS=1 cargo test -p legionctl --test machine the_daemon_serves` |

Read the diff of the file before you commit it. A change that you did not intend is a bug.

## EVIDENCE in the dossier

Write the EVIDENCE when the check runs, from its real output (skill `legionctl:dossier-practice`):

1. `legionctl dossier add EVIDENCE/validation <slug>` with the commands, the counts (passed, failed, ignored), the
   mutation list and the commit SHA. Give it front matter: `id`, `class`, `status`, `code`, `supports`.
2. Set the `plan.md` row to the validated state only with that EVIDENCE.
3. Run `legionctl dossier check`. It reports stale EVIDENCE (code changed after it) and dangling links.

## Commit

1. Commit with `legionctl workspace checkpoint "<message>"` from inside the workspace.
2. Write the message as one long paragraph: what changed and why, the decisions, the tests by name, the mutations,
   what is not tested and why. Look at `git log -5` for the style.
3. End it with the attribution line that the session gives (`Co-Authored-By: ...`).

## CI in batches

The user pays for CI minutes.

1. Commit locally. Push only when the user or the lead says so, with a larger tested batch.
2. Run the local checks above before each push. CI is not the first test run.
3. Follow a run: `gh run list`, then `gh run view <run-id>`.
4. Read a failure: `gh run view <run-id> --log-failed`.
5. A failed step stops the steps after it. When `make ci` fails, the eBPF and VM steps did not run. Do not report
   them as passed.
6. Fix the cause locally, run the local checks again, and push the fix with the next batch.

## Scenario: a new CLI operation

1. `cargo build --release --workspace --bins --locked`
2. Write the e2e test from the CLI boundary. Run it; it fails without the code, passes with it.
3. `LEGIONCTL_WRITE_SCHEMA=1 cargo test -p legionctl --test e2e the_wire_format`, then read the diff.
4. `make static`, then `make lint`, then `cargo test --release --workspace --locked --no-fail-fast`.
5. Mutation-check the new test. Write the list.
6. `legionctl dossier add EVIDENCE/validation <slug>` with the output.
7. `legionctl workspace checkpoint "<message with the tests and mutations>"`.
