# Make targets

`make help` lists them all.

| Target | What it runs |
| --- | --- |
| `make static` | The static `legionctl` (musl) that `workspace pack` puts into agent images, in the `rust:alpine` image with docker. |
| `make lint` | Each pre-commit hook on all files: whitespace, end of file, JSON, TOML, YAML, shell, rustfmt and clippy, Biome, profile validation, actionlint. |
| `make rust-lint` | `cargo fmt --check`, clippy pedantic with warnings as errors, `cargo machete`, `cargo deny check`. |
| `make js-lint` | Biome and tsc for the GNOME Shell extension. |
| `make validate` | Each profile and its shipped configset layers (no hardware access). |
| `make ci` | `lint`, `rust-lint`, `js-lint`, `validate` and `test`: the first CI step. |
