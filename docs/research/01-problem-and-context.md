# The Problem, and Why Existing Tools Do Not Solve It

## The gap

A source repository describes, in machine-readable detail, how to build the
project. It rarely describes what to build it *with*.

`package.json` records which libraries a JavaScript project depends on. It does
not record which Node.js interpreter those libraries were tested against.
`requirements.txt` records Python packages; it does not record the Python.
`go.mod` carries a `go` directive, but that directive states a *language
version* the module is compatible with, not the toolchain a contributor should
install. In every case the runtime — the single largest and most
behaviour-defining dependency — is documented in prose, in a README paragraph,
in a chat message, or not at all.

The observable consequences are mundane and expensive:

- A contributor clones a repository and the build fails for a reason unrelated
  to their change.
- Two developers see different test results, and the difference is a patch
  version of an interpreter neither of them thought to compare.
- Continuous integration passes on a runtime nobody develops against, so CI
  tests a configuration that does not exist on any human's machine.
- Onboarding cost is measured in hours of someone else's time, repeatedly.

This is not a hard problem in the algorithmic sense. It is a problem of *where
the information lives* and *who is responsible for acting on it*.

## Existing approaches and where they stop

### Per-language version managers (nvm, pyenv, rbenv, gvm)

These solve half the problem well. They install multiple versions of one runtime
and switch between them, often reading a dotfile (`.nvmrc`, `.python-version`)
checked into the repository.

Their limits:

- **One language each.** A project with a JavaScript front end and a Python API
  needs two tools with two conventions, two dotfiles, and two installation
  procedures — each of which every contributor must know about.
- **No integrity guarantee.** Most fetch a tarball over HTTPS and unpack it.
  Some check a checksum; the checksum is typically learned from the same host
  that served the artifact, and is not recorded anywhere the team reviews.
- **Shell integration is mandatory.** They work by hooking `cd`, modifying the
  prompt, or shimming binaries, which means installation modifies the user's
  shell configuration and misbehaviour is hard to attribute.
- **No lockfile.** `.nvmrc` containing `22` resolves to a different patch
  release depending on when you read it. That is not reproducibility.

### Polyglot version managers (asdf, mise)

These generalise the above to many languages via a plugin system, which is a
real improvement and the closest neighbour to Kiln in intent.

Their limits, relative to what Kiln attempts:

- **Plugins are shell scripts fetched from third parties.** Installing a runtime
  means executing someone's script with the user's privileges. The trust surface
  is the plugin author, not the runtime publisher.
- **Version resolution is not recorded.** A `.tool-versions` file pins versions,
  which is better than nothing, but the *artifact* — the specific bytes — is not
  pinned, so two installations of "Node 22.14.0" are only as identical as the
  upstream host chooses to make them.
- **Cross-platform pinning is absent.** A developer on macOS cannot produce a
  file that a Linux CI runner installs from without resolving again.

### Nix

Nix solves reproducibility more completely and more rigorously than Kiln does,
and any honest paper must say so. It provides a purely functional package model,
content-addressed storage, exact dependency closures, and reproducible builds
from source.

Its costs, which are the reason people continue not to use it:

- **A new language and a large conceptual model.** Adopting Nix means learning
  Nix. That is a real cost that teams weigh and frequently decline.
- **System-level installation.** The `/nix` store, and historically a daemon and
  build users, are an intrusive change to a machine.
- **All-or-nothing feel.** Partial adoption is possible but uncomfortable; the
  benefits accrue when everything is expressed in Nix.

Kiln is not an attempt to improve on Nix's guarantees. It is an attempt to
capture the most valuable fraction of them at a fraction of the adoption cost.
That trade should be stated plainly rather than obscured.

### Containers and devcontainers

Docker and the devcontainer specification solve the problem by replacing the
environment rather than describing it.

Their costs:

- **A daemon and a virtual machine on non-Linux hosts**, with the attendant
  memory cost and filesystem performance penalty.
- **Editor and tooling friction.** Language servers, debuggers and file watchers
  must be taught to reach inside the container.
- **They solve isolation, which is a different problem.** A developer who wants
  the right Node.js version does not necessarily want a separate filesystem
  namespace, and paying for isolation to get version pinning is a poor exchange.

Containers are the right answer when isolation is the requirement. They are a
heavy answer when reproducibility of the *toolchain* is the requirement.

## The requirements this suggests

Stated as design constraints rather than features:

1. **One declaration for every runtime a project needs**, in one file, in the
   repository, next to the code it describes.
2. **Exact artifacts, not version strings.** Reproducibility means the same
   bytes, which means recording a cryptographic digest, not a version number.
3. **Verification before execution.** Anything installed will be put on a
   developer's `PATH` and run. The digest must be checked before the artifact is
   unpacked, and a mismatch must destroy the bytes rather than warn.
4. **No privileged installation, no daemon, no shell modification.** The tool
   writes to its own directory and nowhere else. Its absence should be as
   uneventful as its presence.
5. **Cross-platform pinning from one machine.** A developer on macOS must be
   able to produce a lockfile a Linux CI runner installs from without resolving
   again — because a CI runner that resolves independently is not running what
   was reviewed.
6. **Offline determinism.** Given a lockfile and a populated cache, installation
   must require no network access at all.
7. **Failure must be legible.** A tool that fails opaquely is a tool people
   route around. Diagnostics are a functional requirement, not presentation.

## Non-goals, stated deliberately

Kiln does not attempt: build reproducibility from source, dependency resolution
for language-level packages (npm and pip remain responsible for those), process
isolation, sandboxing, remote execution, or any hosted component. It has no user
accounts, no telemetry, and no network service.

The narrowness is the design. Each of those would be defensible on its own; each
would also change what the tool can promise to do without supervision.
