# Contributing

**English** | [简体中文](CONTRIBUTING.zh-CN.md)

Thanks for helping. A few rules keep the project maintainable and legally clean.

## Before you start

- Read [`CLEANROOM.md`](CLEANROOM.md).
- For anything larger than a bug fix, open an issue describing the behaviour and
  the evidence source (specification, observation or design).
- Read the architecture overview in [`README.md`](README.md#architecture).

## Development

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Requirements:

- No `unwrap`/`expect` in library crates; return `Result`.
- `#![forbid(unsafe_code)]` stays in every crate that does not explicitly need a
  platform API.
- Layout and export must be deterministic: no clocks, RNG, or hash-map iteration
  order.
- New formatting behaviour needs a test and, when it affects layout, a golden
  fixture.
- Keep crates free of UI and platform dependencies except the designated shell
  and backend crates.

## Pull requests

- One logical change per PR.
- State the evidence source in the description.
- Include tests.
- Do not commit generated files, `target/`, or third-party assets.

## Commit messages

Short imperative subject, optional body explaining the "why". Example:

```
layout: keep closing punctuation on the previous line

Implements the basic 避头 rule from the East Asian line-breaking table.
Spec: UAX #14 section 5.2.
```

## License

By submitting a pull request you agree that your contribution is licensed under
the Apache License 2.0, without additional terms or conditions (Apache-2.0
section 5). See [`LICENSE`](LICENSE).
