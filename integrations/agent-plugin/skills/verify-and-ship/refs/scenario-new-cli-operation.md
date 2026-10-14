# Scenario: a new CLI operation

1. `cargo build --release --workspace --bins --locked`
2. Write the e2e test from the CLI boundary. Run it: it fails without the code and passes with it.
3. `LEGIONCTL_WRITE_SCHEMA=1 cargo test -p legionctl --test e2e the_wire_format`, then read the diff.
4. `make static`, then `make lint`, then `cargo test --release --workspace --locked --no-fail-fast`.
5. Mutation-check the new test. Write the list.
6. `legionctl dossier add EVIDENCE/validation <slug>` with the output.
7. `legionctl workspace checkpoint "<message with the tests and mutations>"`.
