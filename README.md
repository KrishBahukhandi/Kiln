# Kiln

[![CI](https://github.com/KrishBahukhandi/Kiln/actions/workflows/ci.yml/badge.svg)](https://github.com/KrishBahukhandi/Kiln/actions/workflows/ci.yml)
[![Upstream](https://github.com/KrishBahukhandi/Kiln/actions/workflows/upstream.yml/badge.svg)](https://github.com/KrishBahukhandi/Kiln/actions/workflows/upstream.yml)

**Your development environment, committed.**

Define the runtimes, tools and configuration a project needs in one file. Clone
the repository, run Kiln, start building.

> **Status: 0.1.0 — the core loop works.**
> `git clone` → `kiln install` → `kiln shell` → code. Runtimes are resolved,
> verified, installed and put on your `PATH`. What remains is hardening and
> reach; Kiln reports what it cannot do rather than doing something else. See
> [Roadmap](#roadmap) and [What works today](#what-works-today).

---

## Why Kiln exists

A repository already describes how to build the project. It rarely describes
what to build it *with*.

```
Developer A            Developer B
Node 22.14             Node 20
Python 3.13.5          Python 3.11
pnpm 10                pnpm 9
```

`package.json` records dependencies, not the runtime underneath them. The gap is
filled by a README paragraph, a Slack message, or an afternoon.

Kiln closes it with one file that lives in the repository:

```toml
[project]
name = "storefront"

[runtime]
node = "22.14.0"
python = "3.13.5"

[environment]
NODE_ENV = "development"

[commands]
dev = "npm run dev"
```

Anyone who clones the repository gets that environment, and nothing else on
their machine changes.

## Design principles

**Local-first.** No account, no database, no daemon. `kiln install` runs, finishes
and exits.

**Reproducible.** `kiln.toml` says what you want; `kiln.lock` records exactly what
that resolved to, per platform.

**Deterministic.** `node = "22.14.0"` means 22.14.0, not "22.14.0 or later". Kiln
never selects a pre-release you did not ask for, and never quietly changes a
major version.

**Transparent.** Every artifact is verified against a cryptographic digest before
it is installed. Kiln tells you what it did and what it could not do.

**Non-invasive.** Kiln writes to its own store and nowhere else. It does not edit
your shell profile, install anything globally, or ask for `sudo`.

## Installation

Kiln is not yet published to a package registry. Build it from source:

```bash
git clone https://github.com/bahukhandi-labs/kiln
cd kiln
cargo install --path crates/kiln-cli
```

Requires Rust 1.88 or newer. macOS and Linux, on x86-64 or arm64.

## Quick start

```bash
cd your-project
kiln init          # inspects the project and proposes a manifest
kiln install       # resolves, downloads, verifies, and writes kiln.lock
kiln shell         # a shell with the project's runtimes in front
```

```console
$ kiln shell
[kiln] storefront environment activated
  Node.js  22.14.0
  Python   3.13.5

$ node --version
v22.14.0
$ exit
[kiln] left the storefront environment
```

Or run one command without entering a shell:

```bash
kiln run node --version
kiln run npm install
kiln run test              # a command from [commands]
```

Commit both `kiln.toml` and `kiln.lock`. A colleague who clones the repository
and runs `kiln install` gets byte-identical runtimes.

## What works today

| | Command | |
| --- | --- | --- |
| ✅ | `kiln init` | Detects the project and writes `kiln.toml`. |
| ✅ | `kiln install` | Resolves, downloads, verifies, installs, writes `kiln.lock`. `--locked` for CI, `--jobs N` to fetch several at once. |
| ✅ | `kiln lock` | Writes `kiln.lock` without installing. `--all-platforms`, `--check`. |
| ✅ | `kiln list` | What the project pins and whether it is installed. `--json`. |
| ✅ | `kiln doctor` | Diagnoses the project and this machine. `--json`. |
| ✅ | `kiln run` | Runs a command in the environment. Transparent exit codes. |
| ✅ | `kiln shell` | A shell with the project's runtimes in front. |
| ✅ | `kiln cache list` | What is in the store and where it came from. `--json`. |
| ✅ | `kiln cache clean` | Evicts runtimes nothing has used recently. `--force` to delete. |
| ✅ | `kiln clean` | Removes scratch space and cached indexes. `--force` to delete. |
| ✅ | `kiln version` | Version, platform and store location. `--json`. |
| ✅ | `kiln cache verify` | Re-reads the store and reports any runtime that changed. `--json`. |

Every command in the tree is implemented. Nothing exits with "not implemented",
and nothing pretends to succeed.

### Runtimes

| Runtime | Source | Platforms |
| --- | --- | --- |
| Node.js | [nodejs.org](https://nodejs.org/dist) | macOS and Linux, x86-64 and arm64 (glibc only — upstream publishes no musl builds) |
| Python | [python-build-standalone](https://github.com/astral-sh/python-build-standalone) | macOS and Linux, x86-64 and arm64, glibc **and** musl |
| Go | [go.dev](https://go.dev/dl) | macOS and Linux, x86-64 and arm64, glibc and musl (the toolchain is statically linked) |
| Deno | [dl.deno.land](https://dl.deno.land) | macOS and Linux, x86-64 and arm64 (glibc only — upstream publishes no musl builds) |

Adding one is a single file plus a line in the registry — see
[CONTRIBUTING.md](CONTRIBUTING.md).

## `kiln.toml`

```toml
[project]
name = "example-app"          # required
version = "0.1.0"             # optional, not interpreted by Kiln
description = "..."           # optional

[runtime]                     # language runtimes
node = "22.14.0"
python = "3.13.5"
go = "1.25"

[tools]                       # tools that ride on a runtime
pnpm = "10.12.1"

[environment]                 # exported into the project environment
NODE_ENV = "development"

[commands]                    # run with `kiln run <name>`
dev = "npm run dev"
test = "npm test"

[services]                    # parsed and reported; not managed yet
postgres = "17"
```

A full annotated example is in [`kiln.example.toml`](kiln.example.toml).

### Version requirements

| Written | Means |
| --- | --- |
| `22.14.0` | exactly 22.14.0 |
| `22` | any 22.x.x |
| `22.14` | any 22.14.x |
| `^22.14` | `>=22.14.0`, `<23.0.0` |
| `~22.14` | `>=22.14.0`, `<22.15.0` |
| `>=22, <23` | every comparator must hold |
| `lts`, `latest` | resolved by the runtime provider, then locked |

Note the first row. Cargo would read `22.14.0` as "22.14.0 or any compatible
later release"; Kiln reads it as 22.14.0. Reproducibility beats convenience.

Pre-releases are never selected unless the requirement names one explicitly.
`*` is rejected — write `latest` if you mean it, and the lockfile will record
what it resolved to.

### Commands do not run through a shell

`kiln run dev` executes the program directly. `&&`, `|`, `>` and `$` are not
interpreted, and a manifest that uses them is rejected at parse time rather than
silently doing something different from what it looks like it does.

Arguments after a command name are appended, so `kiln run test --watch` does
what `npm run test -- --watch` does, without the `--`.

```toml
[commands]
dev = "npm run dev"                       # fine
build = "npm run build && npm run dev"    # rejected
release = "./scripts/release.sh"          # the way to use shell logic
```

This is also why cloning a repository and running `kiln install` cannot execute
code the repository chose.

### `PATH` is Kiln's

`[environment]` cannot set `PATH`, `LD_PRELOAD` or `DYLD_INSERT_LIBRARIES`. The
first would disable the environment the project just asked for; the others let a
cloned repository inject a library into every process you run.

## Errors

Diagnostics are a feature, not an afterthought. Every failure says what happened,
where, why, what Kiln expected, and what to do:

```
error: Invalid kiln.toml

  ┌─ kiln.toml:5:8
  │
5 │ node = "banana"
  │        ^^^^^^^^ unsupported version requirement `banana`: `banana` is not a number

  Expected:
    an exact version        22.14.0
    a major or minor pin    22          22.14
    a caret range           ^22.14
    a tilde range           ~22.14
    a comparator range      >=22, <23
    a supported alias       lts         latest
```

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | success |
| 2 | invalid configuration |
| 3 | not found (no project, unknown runtime) |
| 4 | unsupported platform |
| 5 | network failure |
| 6 | artifact failed verification |
| 7 | filesystem failure |
| 8 | conflicts with existing state |
| 9 | not implemented in this release |
| 70 | internal error — please report it |
| 130 | interrupted |

## The store

Artifacts are stored under the digest of the archive they came from:

```
~/.kiln/
  store/sha256/e9/e9404633bc02a516…/
    meta.toml                           where it came from
    content/                            the unpacked runtime
  staging/                              in-flight work, never visible as installed
  state/                                cached release indexes; safe to delete
```

Two projects needing Node.js 22.14.0 share one directory without knowing about
each other. Installing it a second time downloads nothing.

Every artifact is hashed **as it downloads** and compared against the digest its
publisher declared, before anything is unpacked. A mismatch discards the bytes
and installs nothing. The URL in `kiln.lock` is advisory; the digest is what is
trusted, so a mirror is fine and different bytes are not.

`KILN_HOME` relocates the whole tree, which is useful in CI.

### Reclaiming disk

```bash
kiln cache clean                      # what has been idle 30+ days
kiln cache clean --force              # actually remove it
kiln cache clean --older-than 7 --force
```

Kiln keeps no registry of projects — that is deliberate — so it cannot know
which entries some other checkout still needs. It records when each entry was
last *used* instead, and evicts by age. Being wrong is cheap in only one
direction: removing something still wanted costs a re-download, so the default
is conservative and deleting takes `--force`.

Use is tracked by Kiln rather than read from the filesystem's access time,
which is unreliable in practice — `relatime` is the Linux default and `noatime`
is common, so a runtime you use daily can look untouched. `kiln run` and
`kiln shell` record use; read-only commands like `kiln list` do not.

## `kiln run` and `kiln shell`

Both put the project's runtimes at the front of `PATH` and leave the rest of it
alone, so `git`, `ssh` and your editor keep working. Nothing is written to a
shell profile; `kiln shell` starts a child shell and the environment ends when
that shell exits.

`kiln run` is transparent on purpose: streams are inherited, arguments are
passed through untouched, and the child's exit code becomes Kiln's. It prints
nothing of its own, so `kiln run npm test` can stand in for `npm test` in a
Makefile or a CI step without anything downstream noticing.

Neither command installs anything. If a runtime is missing they say so and point
at `kiln install`, rather than downloading fifty megabytes because you asked to
run a one-line script.

Kiln resolves the program against the composed `PATH` itself rather than relying
on the operating system, because `Command::env("PATH", …)` does not reliably
affect program lookup — and "the project's runtime wins" is the one guarantee
that must not depend on unspecified behaviour.

`kiln shell` is a `PATH` change, not a sandbox. It shadows what a project pins;
it does not take your machine away.

## Teams and CI

`kiln.lock` is keyed by platform, and one person can fill in every platform from
whatever machine they happen to have:

```console
$ kiln lock --all-platforms
◆ Kiln lock

Resolving
  macos-aarch64
    Node.js  22.14.0  resolved
    Python   3.13.15  resolved
  linux-x86_64-gnu
    Node.js  22.14.0  resolved
    Python   3.13.15  resolved
  …

Skipped
  linux-x86_64-musl    Node.js is not available for Linux x86_64 (musl)

✓ Wrote kiln.lock for 4 platforms
```

Resolving for a platform does not require *being* on it — a provider is handed
the platform it is resolving for, never the host it runs on. Skipped platforms
are reported rather than fatal: "this project cannot run on Alpine" is
information, not a failure of the lock command.

Commit the result. A CI runner on another operating system then installs from it
without re-resolving anything.

### `--locked`

```bash
kiln install --locked      # fail rather than change kiln.lock
kiln lock --check          # fail if kiln.lock is out of date, write nothing
```

`--locked` is what makes Kiln safe to run in CI: it guarantees the environment
installed is the one that was reviewed. The check is structural and offline, so
it fails *before* anything is resolved or downloaded:

```
error: kiln.lock does not cover macOS arm64, and `--locked` was given

  Kiln would have to change the lockfile to continue:
    node is locked for `22.14.0`, but kiln.toml asks for `22`

  Try:
    • run `kiln lock` and commit the result
    • drop `--locked` to update it as part of installing
```

A worked example:

```yaml
# .github/workflows/ci.yml
- run: kiln lock --check           # the lockfile matches the manifest
- run: kiln install --locked       # install exactly what was reviewed
- run: kiln run test
```

Combine `--locked` with `--offline` to forbid both lockfile changes and network
access — useful when the store is restored from a cache.

## Offline

Kiln goes to the network only when it has to.

- A `kiln.lock` entry that still matches the manifest skips resolution entirely.
- An exact pin like `node = "22.14.0"` skips the release index — it needs one
  small checksum file, not a quarter-megabyte of JSON.
- Anything already in the store is never re-downloaded.

So a pinned project with a committed lockfile installs with no network at all:

```bash
kiln install --offline
```

`--offline` refuses to make any request and says exactly what is missing rather
than hanging on a connection that cannot succeed.

## Output

- **stdout carries data** — tables, JSON, and the output of anything Kiln runs on
  your behalf. `kiln cache list --json | jq` works.
- **stderr carries narration** — progress, summaries, warnings and errors.

`--quiet` silences narration but never errors. `--color never|always|auto`
controls escapes; `NO_COLOR` is honoured.

## Architecture

```
CLI  →  Config  →  Resolver  →  Runtime Provider
                       ↓              ↓
                   Lockfile       Artifact
                                      ↓
                                 Content Store
                                      ↓
                                 Environment  →  Process
```

Eight crates, split by responsibility rather than by convenience:

| Crate | Owns |
| --- | --- |
| `kiln-core` | Errors, versions, platforms, digests, paths, output. Depends on no other Kiln crate. |
| `kiln-config` | The `kiln.toml` schema, parsing, validation, discovery, generation. |
| `kiln-net` | The only crate allowed to make an HTTP request. |
| `kiln-cache` | The content-addressed store, and archive extraction. |
| `kiln-runtime` | `RuntimeProvider`, and everything specific to a particular runtime. |
| `kiln-resolver` | Manifest → exact environment → installed, and the lockfile. |
| `kiln-exec` | Composing `PATH`, resolving programs, running them. |
| `kiln-cli` | Argument parsing and command dispatch. |

Confining HTTP to one crate means "does this command need the network?" is a
question you can answer by reading the dependency graph.

`docs/architecture.md` explains the boundaries and why they are where they are.

## Roadmap

| Phase | Scope | Status |
| --- | --- | --- |
| 0 | Workspace, errors, config model, CLI skeleton | ✅ done |
| 1 | `kiln init`, validation, project discovery | ✅ done |
| 2 | Node.js and Python providers: resolve, download, verify, install | ✅ done |
| 3 | Cache garbage collection | ✅ done |
| 3 | Cache verification | ✅ done |
| 8 | More runtimes | Go and Deno done; Bun and Rust next |
| 4 | `kiln shell`, `kiln run` | ✅ done |
| 5 | `kiln clean`, richer output | partial |
| 6 | Cross-platform locking, `--locked` for CI, drift reporting | ✅ done |
| 7 | Concurrent downloads (`--jobs`), mirrors, signatures | concurrency done, off by default; mirrors and signatures next |
| 8 | Services, OCI, Windows, IDE integration | |

Explicitly **not** planned for 1.0: user accounts, a cloud dashboard, telemetry,
remote execution, or replacing containers.

### Telemetry

There is none, and none is planned. If that ever changes it will be opt-in,
documented, and disableable.

## The website

`website/` holds the marketing and documentation site — React, TypeScript, Vite
and Tailwind, entirely independent of the Rust workspace.

It has its own `kiln.toml`, so Kiln builds its own site:

```bash
cd website
kiln install
kiln run build
```

Every terminal block on the page is output the tool actually produces. There are
no images, no web fonts and no third-party requests — a site arguing for fewer
dependencies should not open with three of them.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Adding a runtime means writing one
provider and adding one line to the registry.

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The default test suite never touches the network. Tests that really download
from nodejs.org and python-build-standalone are `#[ignore]`d:

```bash
cargo test -p kiln-cli --test network -- --ignored --test-threads=1
```

## Security

Kiln downloads and executes binaries, so integrity is not optional. See
[SECURITY.md](SECURITY.md) for the threat model and how to report a
vulnerability.

## License

MIT. See [LICENSE](LICENSE).
