# Contributing to Kiln

## Getting set up

```bash
git clone https://github.com/bahukhandi-labs/kiln
cd kiln
cargo test --workspace
```

Rust 1.88 or newer. The default test suite runs offline and never touches your
real `~/.kiln` — every test gets its own `KILN_HOME`, and the integration
sandbox passes `--offline` so a test cannot reach nodejs.org by accident.

Tests that genuinely exercise the download path are `#[ignore]`d:

```bash
cargo test -p kiln-cli --test network -- --ignored --test-threads=1
```

Run them before any change to a provider, the download path or extraction. They
take a couple of minutes and are the only thing that catches an upstream whose
format has changed.

## Before you open a pull request

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All three must be clean. Warnings get fixed, not allowed.

## What the codebase expects

**Errors are a product surface.** Every failure a user can reach should answer
four questions: what happened, why, what Kiln expected, and what to do next.

```rust
Error::not_found(format!("Kiln does not know the runtime `{name}`"))
    .because("no provider in this build of Kiln can install it")
    .expected(format!("one of: {}", registry.ids().join(", ")))
    .hint(format!("did you mean `{suggestion}`?"))
```

Never `Error: something went wrong`. Never a panic on a path a user can reach:
`unwrap` and `expect` are for invariants a test already guarantees, and the
`expect` message should say which test.

**Validate in `Deserialize`.** Configuration values are checked inside their
`Deserialize` implementations, because that is what makes the TOML parser hand
back the byte span of the offending token — which is what puts a caret under it.
A check moved out to a later pass loses its source location.

**Nothing runtime-specific outside `kiln-runtime`.** If you find yourself writing
`if runtime == "node"` anywhere else, the logic belongs on `RuntimeProvider`.

**`BTreeMap`, not `HashMap`,** for anything that reaches output or a lockfile.
Resolution must be a function of the manifest, not of iteration order.

**Do not fake a feature.** If something is not built, it fails with
`Error::not_implemented` naming its phase. A stub that silently succeeds is worse
than an honest failure, and a trait method that exists only to return
`NotImplemented` is a design guess every implementor will inherit.

## Adding a runtime

This is the path the architecture is built around, and it should stay short.

1. Add `crates/kiln-runtime/src/providers/<name>.rs` implementing
   `RuntimeProvider`: identity, `supports`, `detect`, `releases`, `artifact` and
   `layout`.
2. Register it in `Registry::builtin`.
3. Add unit tests for detection *and* for parsing the upstream release index —
   use a slice of the real index as a fixture, including the awkward entries.
   The `lts` field on nodejs.org is `false` or a string in the same array; that
   kind of thing is what a hand-written fixture misses.
4. Add an `#[ignore]`d test in `crates/kiln-cli/tests/network.rs` that installs
   the runtime and executes the resulting binary.

Note what you do *not* write: downloading, verifying and extracting are shared.
A provider declares a `RuntimeLayout` and the installer does the rest, so there
is exactly one piece of tar-handling code in Kiln to get right.

That is the whole change. Nothing in `kiln-config`, `kiln-resolver` or
`kiln-cli` should need touching — if it does, the abstraction has a hole and the
hole is the bug.

## Tests

- **Unit tests** live in the module they test, in a `#[cfg(test)] mod tests`.
- **Integration tests** are in `crates/kiln-cli/tests/cli.rs` and drive the real
  binary.
- **Fixtures** are in `tests/fixtures/`. Each invalid manifest contains exactly
  one mistake, so a test asserting a particular diagnostic cannot pass because of
  a different error in the same file.

Name tests after the behaviour they pin down, not the function they call:
`a_bad_version_points_a_caret_at_the_value`, not `test_parse_error`.

Tests that need the network must be `#[ignore]`d. `cargo test` has to work on a
plane.

## Dependencies

The tree is small on purpose. A new dependency needs a reason in the pull
request, and "it saves twenty lines" usually is not one. Prefer adding a
dependency when the code that uses it lands, not before.

## Commits

Present tense, describing the change: `reject shell operators in commands`, not
`fixed stuff`. If a change alters behaviour a user can see, update the README in
the same commit.

## Where to start

Phase 3 is the next milestone: cache verification and garbage collection. Both
need a canonical way to hash a directory tree, and that decision fixes the
meaning of "this entry is intact" permanently — so it is worth discussing in an
issue before any code is written. [`docs/architecture.md`](docs/architecture.md)
describes the constraints.

Garbage collection additionally needs an answer to "which store entries are
still reachable?", and the honest answer spans projects a single command cannot
see. Worth designing before building.
