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
is exactly one piece of archive-handling code in Kiln to get right.

That is the whole change. Nothing in `kiln-config`, `kiln-resolver` or
`kiln-cli` should need touching — if it does, the abstraction has a hole and the
hole is the bug.

### What the last two taught us

**Go** took one file plus one registry line, and surfaced a bug elsewhere. `go`
is two characters, and the "did you mean?" suggestion used a fixed two-edit
budget, so it started proposing `go` for `io`, `ai` and the empty string. Short
names now match on shared prefix only. A runtime with an unusually short or long
name is worth a glance at `Registry::suggest`.

**Deno** needed two things the abstraction did not have, which is the honest
version of "adding a runtime is one file":

- It ships zip and nothing else, so `kiln-cache` gained a zip reader.
- It is a single binary at the archive root rather than a tree with a `bin/`,
  so `RuntimeLayout` gained `FLAT` and `bin_paths`.

Both are now shared, so the next runtime shaped like either gets them for free.
The lesson is the one in the last paragraph: when a provider cannot be written
without reaching outside its own file, the gap belongs in the shared layer, not
in the provider.

### Before you pick a runtime, check what it publishes

Two candidates that look easy and are not:

- **Bun** ships zip, which is now fine, but its only version index is the GitHub
  releases API — sixty requests an hour unauthenticated, shared across everyone
  behind one NAT. Deno avoids this entirely with `dl.deno.land/versions.json`.
  Someone should work out what Kiln does when that budget runs out before
  writing the provider, not after.
- **Rust** publishes a combined `tar.gz`, so extraction is not the problem. The
  problem is that it unpacks to per-component directories — `cargo/bin`,
  `rustc/bin`, `rust-std-<triple>/` — and `rustc` finds its standard library
  relative to its own path. Unpacked as-is it installs cleanly and then cannot
  compile anything, which is the worst failure shape there is. It needs either
  component merging or `install.sh`, and running a vendor's shell script is a
  different trust model than Kiln has anywhere else. Decide that first.

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

## The website

`website/` is a separate project with its own `kiln.toml` and its own
`package.json`. It never imports from the Rust workspace and the workspace never
knows it exists — `cargo test` does not build it, and changing it cannot break
the CLI.

```bash
cd website
kiln install        # Kiln installs the Node the site is pinned to
kiln run build
```

If you change something a user sees, check the site still tells the truth. The
terminal blocks are real output, and they are the most convincing thing on the
page precisely because they are.

## Dependencies

The tree is small on purpose. A new dependency needs a reason in the pull
request, and "it saves twenty lines" usually is not one. Prefer adding a
dependency when the code that uses it lands, not before.

## Commits

Present tense, describing the change: `reject shell operators in commands`, not
`fixed stuff`. If a change alters behaviour a user can see, update the README in
the same commit.

## Where to start

Every command in the tree is implemented, so the open work is depth rather than
breadth. In rough order of how much it is missed:

- **More runtimes.** Rust, Bun and Deno. One file each, and the fastest way to
  learn the codebase — see "Adding a runtime" above.
- **Publisher signatures.** The largest real gap. Kiln verifies digests, which
  makes it trust-on-first-use; see [`SECURITY.md`](SECURITY.md). This needs a
  design discussion before code, because where a signature is learned from
  matters more than how it is checked.
- **Services.** `[services]` in `kiln.toml` is parsed and reported but nothing
  manages it. Doing it properly means container lifecycle, ports and health
  checks — a phase, not a patch.
- **Windows.** `Os::Windows` exists so lockfiles stay portable across a team,
  but no provider builds for it, and `.zip` handling and `PATH` semantics are
  both unwritten.

Two decisions are already made and should not be relitigated without a reason:
garbage collection evicts by last use rather than by reachability, and
verification compares against a manifest recorded at install rather than a hash
of the tree. [`docs/architecture.md`](docs/architecture.md) says why for both.
