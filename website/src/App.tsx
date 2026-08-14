import {
  A,
  C,
  Cmd,
  Comment,
  FileBlock,
  Key,
  Out,
  Point,
  Section,
  Str,
  Terminal,
} from "./components";

const REPO = "https://github.com/KrishBahukhandi/Kiln";

/**
 * The Kiln site.
 *
 * One page. Everything a developer needs to decide whether this tool is worth
 * their afternoon, in the order they will ask for it: what problem, what it
 * looks like, whether it is trustworthy, and what it cannot do yet.
 *
 * Every terminal block on this page is output the tool actually produces.
 */
export default function App() {
  return (
    <>
      <a
        href="#top"
        className="sr-only focus:not-sr-only focus:absolute focus:left-4 focus:top-4 focus:z-50 focus:rounded focus:bg-ink focus:px-4 focus:py-2 focus:text-paper"
      >
        Skip to content
      </a>

      <Masthead />
      <Hero />
      <Why />
      <OneFile />
      <Reproducible />
      <Store />
      <NoDaemon />
      <WorksWith />
      <Architecture />
      <Commands />
      <Status />
      <Footer />
    </>
  );
}

function Masthead() {
  const links = [
    ["Why", "#why"],
    ["One file", "#one-file"],
    ["Cache", "#cache"],
    ["Architecture", "#architecture"],
    ["Commands", "#commands"],
    ["Status", "#status"],
  ];
  return (
    <header className="sticky top-0 z-40 border-b border-rule bg-paper/85 backdrop-blur">
      <div className="mx-auto flex max-w-5xl items-center justify-between px-6 py-3.5">
        <a href="#top" className="flex items-center gap-2.5">
          <Mark />
          <span className="font-semibold tracking-tight">Kiln</span>
        </a>
        <nav className="hidden items-center gap-6 text-sm text-ink-muted md:flex">
          {links.map(([label, href]) => (
            <a key={href} href={href} className="transition-colors hover:text-ink">
              {label}
            </a>
          ))}
        </nav>
        <a
          href={REPO}
          target="_blank"
          rel="noreferrer noopener"
          className="rounded-md border border-rule px-3 py-1.5 text-sm font-medium transition-colors hover:border-ink-faint"
        >
          GitHub
        </a>
      </div>
    </header>
  );
}

function Mark() {
  return (
    <svg width="22" height="22" viewBox="0 0 32 32" aria-hidden className="shrink-0">
      <rect width="32" height="32" rx="7" className="fill-ink" />
      <path
        d="M11 8v16M21 8l-8 8 8 8"
        className="stroke-ember-bright"
        strokeWidth="2.5"
        fill="none"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function Hero() {
  return (
    <div id="top" className="mx-auto max-w-5xl px-6 pb-16 pt-20 sm:pt-28">
      <p className="font-mono text-xs uppercase tracking-[0.18em] text-ember">
        Reproducible development environments
      </p>

      <h1 className="mt-6 max-w-3xl text-4xl font-semibold leading-[1.08] tracking-tight sm:text-6xl">
        {/* The space before the break matters: `textContent` ignores <br>, so
            without it the heading copies and reads as "developmentenvironment". */}
        Your development{" "}
        <br />
        environment, committed.
      </h1>

      <p className="mt-7 max-w-2xl text-lg leading-relaxed text-ink-muted sm:text-xl">
        Define runtimes and tools in one file. Clone the repository and reproduce
        the environment anywhere — down to the exact bytes, verified on the way in.
      </p>

      <div className="mt-9 flex flex-wrap items-center gap-3">
        <a
          href="#one-file"
          className="rounded-md bg-ink px-5 py-2.5 text-sm font-medium text-paper transition-opacity hover:opacity-85"
        >
          Get started
        </a>
        <a
          href={REPO}
          target="_blank"
          rel="noreferrer noopener"
          className="rounded-md border border-rule px-5 py-2.5 text-sm font-medium transition-colors hover:border-ink-faint"
        >
          View on GitHub
        </a>
      </div>

      <div className="mt-14">
        <Terminal title="storefront">
          <Cmd>git clone git@github.com:acme/storefront.git &amp;&amp; cd storefront</Cmd>
          <Cmd>kiln install</Cmd>
          {"\n"}
          <Out tone="faint">Resolving</Out>
          <Out>{"  Node.js     22.14.0      from kiln.lock"}</Out>
          <Out>{"  Python      3.13.15      from kiln.lock"}</Out>
          {"\n"}
          <Out tone="faint">Installing</Out>
          <Out tone="ok">{"  ✓ Node.js 22.14.0          already in the store"}</Out>
          <Out tone="ok">{"  ✓ Python 3.13.15           already in the store"}</Out>
          {"\n"}
          <Out tone="ok">✓ Environment ready</Out>
          {"\n"}
          <Cmd>kiln shell</Cmd>
          <Out tone="accent">[kiln] storefront environment activated</Out>
          <Out tone="faint">{"  Node.js  22.14.0"}</Out>
          <Out tone="faint">{"  Python   3.13.15"}</Out>
          {"\n"}
          <Cmd>node --version</Cmd>
          <Out>v22.14.0</Out>
        </Terminal>
        <p className="mt-4 font-mono text-xs text-ink-faint">
          Two runtimes, no network, 2 ms — the store already had them.
        </p>
      </div>
    </div>
  );
}

function Why() {
  return (
    <Section
      id="why"
      index="01"
      title="Why Kiln exists"
      lede={
        <>
          A repository already describes how to build the project. It rarely
          describes what to build it <em>with</em>.
        </>
      }
    >
      <div className="grid gap-10 md:grid-cols-[1.1fr_1fr] md:gap-14 [&>*]:min-w-0">
        <div className="space-y-5 text-[15px] leading-relaxed text-ink-muted">
          <p>
            <C>package.json</C> records dependencies, not the runtime underneath
            them. So the runtime lives in a README paragraph, a Slack message, or
            an afternoon of someone else's time.
          </p>
          <p>
            Kiln closes the gap with one file that lives in the repository next to
            the code it describes. Clone, install, start working — and the versions
            you get are the versions that were reviewed.
          </p>
          <p className="text-ink">
            Nothing else on your machine changes. Kiln writes to its own store and
            nowhere else. No <C>sudo</C>, no shell profile edits, no daemon.
          </p>
        </div>

        <div className="rounded-lg border border-rule bg-paper-sunk p-5">
          <p className="font-mono text-xs uppercase tracking-wider text-ink-faint">
            The usual Monday
          </p>
          <div className="mt-4 grid grid-cols-2 gap-x-6 font-mono text-sm">
            <div>
              <p className="text-xs text-ink-faint">Developer A</p>
              <ul className="mt-2 space-y-1 text-ink">
                <li>Node 22.14</li>
                <li>Python 3.13.5</li>
                <li>Go 1.26</li>
              </ul>
            </div>
            <div>
              <p className="text-xs text-ink-faint">Developer B</p>
              <ul className="mt-2 space-y-1 text-ink">
                <li>Node 20</li>
                <li>Python 3.11</li>
                <li>Go 1.22</li>
              </ul>
            </div>
          </div>
          <p className="mt-5 border-t border-rule pt-4 text-sm leading-relaxed text-ink-muted">
            Same repository. Same branch. Different failures — and an afternoon
            spent finding out which of the six is the one that matters.
          </p>
        </div>
      </div>
    </Section>
  );
}

function OneFile() {
  return (
    <Section
      id="one-file"
      index="02"
      title="One file, one environment"
      lede={
        <>
          <C>kiln.toml</C> is the whole interface. <C>kiln init</C> writes a first
          draft by reading what your project already says.
        </>
      }
    >
      <div className="grid gap-8 lg:grid-cols-2 [&>*]:min-w-0">
        <FileBlock name="kiln.toml">
          <Comment>{"# Commit this. Everyone gets the same environment.\n\n"}</Comment>
          {"[project]\n"}
          <Key>name</Key>
          {" = "}
          <Str>"storefront"</Str>
          {"\n\n[runtime]\n"}
          <Key>node</Key>
          {"   = "}
          <Str>"22.14.0"</Str>
          {"\n"}
          <Key>python</Key>
          {" = "}
          <Str>"3.13"</Str>
          {"\n"}
          <Key>go</Key>
          {"     = "}
          <Str>"1.26"</Str>
          {"\n\n[environment]\n"}
          <Key>NODE_ENV</Key>
          {" = "}
          <Str>"development"</Str>
          {"\n\n[commands]\n"}
          <Key>dev</Key>
          {"  = "}
          <Str>"npm run dev"</Str>
          {"\n"}
          <Key>test</Key>
          {" = "}
          <Str>"npm test"</Str>
          {"\n"}
        </FileBlock>

        <div className="space-y-6">
          <Point title="Kiln reads what is already there">
            <C>kiln init</C> finds <C>package.json</C>, <C>.nvmrc</C>,{" "}
            <C>pyproject.toml</C>, <C>go.mod</C> and the rest, proposes a manifest,
            and shows it to you before writing anything.
          </Point>
          <Point title="Compatibility is not a pin">
            <C>engines.node: "&gt;=22"</C> says what a project tolerates, not what
            it should be built against. Copied verbatim it would resolve to a new
            major every year, so Kiln proposes <C>node = "22"</C> and says it did.
          </Point>
          <Point title="Commands never see a shell">
            <C>kiln run dev</C> executes the program directly. <C>&amp;&amp;</C>,{" "}
            <C>|</C> and <C>$</C> are rejected when the manifest is parsed — which
            is also why cloning a repository cannot run code it chose.
          </Point>
        </div>
      </div>
    </Section>
  );
}

const VERSION_RULES: [string, string][] = [
  ["22.14.0", "exactly 22.14.0"],
  ["22", "any 22.x.x"],
  ["22.14", "any 22.14.x"],
  ["^22.14", ">=22.14.0, <23.0.0"],
  ["~22.14", ">=22.14.0, <22.15.0"],
  [">=22, <23", "every comparator must hold"],
  ["lts", "resolved by the provider, then locked"],
];

function Reproducible() {
  return (
    <Section
      id="reproducible"
      index="03"
      title="Reproducible runtimes"
      lede="A version requirement should mean one thing. Kiln owns its grammar rather than borrowing one whose defaults are wrong for this problem."
    >
      <div className="grid gap-10 lg:grid-cols-[1fr_1fr] lg:gap-14 [&>*]:min-w-0">
        <div className="overflow-x-auto">
          <table className="w-full min-w-[22rem] text-left font-mono text-sm">
            <thead>
              <tr className="border-b border-rule text-xs uppercase tracking-wider text-ink-faint">
                <th className="py-2 font-normal">Written</th>
                <th className="py-2 font-normal">Means</th>
              </tr>
            </thead>
            <tbody>
              {VERSION_RULES.map(([written, means]) => (
                <tr key={written} className="border-b border-rule/60">
                  <td className="py-2.5 pr-4 text-ember">{written}</td>
                  <td className="py-2.5 text-ink-muted">{means}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>

        <div className="space-y-6">
          <Point title="The first row is the whole argument">
            Cargo reads <C>22.14.0</C> as "22.14.0 or any compatible later
            release". Kiln reads it as 22.14.0. Reproducibility beats convenience,
            so the mapping from text to meaning is explicit.
          </Point>
          <Point title="No accidental release candidates">
            A pre-release is never selected unless the requirement names one.{" "}
            <C>*</C> is rejected outright — it is not a requirement, it is the
            absence of one.
          </Point>
          <Point title="kiln.lock records what it resolved to">
            The manifest may be loose. The lockfile never is. It is keyed by
            platform, so one developer can lock for the whole team from whatever
            machine they have:
          </Point>
          <Terminal>
            <Cmd>kiln lock --all-platforms</Cmd>
            <Out tone="faint">{"  macos-aarch64      Node.js 22.14.0  Python 3.13.15"}</Out>
            <Out tone="faint">{"  macos-x86_64       Node.js 22.14.0  Python 3.13.15"}</Out>
            <Out tone="faint">{"  linux-x86_64-gnu   Node.js 22.14.0  Python 3.13.15"}</Out>
            <Out tone="faint">{"  linux-aarch64-gnu  Node.js 22.14.0  Python 3.13.15"}</Out>
            <Out tone="ok">✓ Wrote kiln.lock for 4 platforms</Out>
          </Terminal>
        </div>
      </div>
    </Section>
  );
}

function Store() {
  return (
    <Section
      id="cache"
      index="04"
      title="Content-addressed cache"
      lede="Every artifact is stored under the digest of its contents, never under a name someone chose."
    >
      <div className="grid gap-10 lg:grid-cols-[1fr_1fr] lg:gap-14 [&>*]:min-w-0">
        <div>
          <FileBlock name="~/.kiln">
            {"store/sha256/e9/e9404633bc02a516…/\n"}
            <Comment>{"  meta.toml      where it came from\n"}</Comment>
            <Comment>{"  content/       the unpacked runtime\n"}</Comment>
            {"staging/"}
            <Comment>{"         in flight, never visible as installed\n"}</Comment>
            {"state/"}
            <Comment>{"           cached indexes; safe to delete\n"}</Comment>
          </FileBlock>

          <div className="mt-6">
            <Terminal title="a lockfile pointing at the wrong bytes">
              <Out tone="bad">error: Node.js 22.14.0 failed verification</Out>
              {"\n"}
              <Out tone="faint">
                {"  The bytes from nodejs.org do not match the digest Kiln required."}
              </Out>
              <Out tone="faint">{"  Kiln has discarded them and installed nothing."}</Out>
              {"\n"}
              <Out tone="faint">{"  Expected:"}</Out>
              <Out tone="faint">{"    expected  sha256:e9404633bc02a516…"}</Out>
              <Out tone="faint">{"    received  sha256:6698587713ab565a…"}</Out>
            </Terminal>
          </div>
        </div>

        <div className="space-y-6">
          <Point title="Sharing happens without coordination">
            Two projects needing byte-identical copies of Node.js 22.14.0 converge
            on one directory without either knowing the other exists. Installing it
            a second time downloads nothing.
          </Point>
          <Point title="Verified before it is unpacked">
            Bytes are hashed as they arrive and compared against the digest the
            publisher declared. A mismatch discards the download and installs
            nothing. The URL is advisory; the digest is what is trusted, so a
            mirror is fine and different bytes are not.
          </Point>
          <Point title="Nothing half-written is ever visible">
            Downloads land in <C>staging/</C> and are promoted into the store with
            a single atomic rename. An install interrupted at any earlier point
            leaves the store exactly as it was.
          </Point>
          <Point title="Disk comes back">
            <C>kiln cache clean</C> evicts runtimes nothing has used recently. Kiln
            keeps no registry of projects — deliberately — so it records use itself
            rather than trusting filesystem access times, which are unreliable
            under <C>relatime</C>.
          </Point>
        </div>
      </div>
    </Section>
  );
}

function NoDaemon() {
  return (
    <Section
      id="no-daemon"
      index="05"
      title="No daemon"
      lede="Kiln is a program that runs, finishes, and exits. Nothing stays behind."
    >
      <div className="grid gap-6 sm:grid-cols-3 [&>*]:min-w-0">
        <Point title="Nothing in the background">
          No service to start, supervise, or debug at 3am. When a command exits,
          Kiln is not running.
        </Point>
        <Point title="Nothing in your shell profile">
          <C>kiln shell</C> starts a child shell with a modified environment and
          waits. Leaving the shell leaves no trace.
        </Point>
        <Point title="Nothing sent anywhere">
          No account, no telemetry, and none planned. Kiln talks to the runtime
          vendors and to nobody else.
        </Point>
      </div>

      <div className="mt-10">
        <Terminal>
          <Cmd>kiln run npm test</Cmd>
          <Out tone="faint">{"…the output of npm test, and nothing else"}</Out>
          {"\n"}
          <Cmd>echo $?</Cmd>
          <Out>1</Out>
        </Terminal>
        <p className="mt-4 max-w-2xl text-[15px] leading-relaxed text-ink-muted">
          <C>kiln run</C> is transparent on purpose: streams are inherited,
          arguments pass through untouched, and the child's exit code becomes
          Kiln's. It prints nothing of its own, so it can stand in for the command
          it wraps anywhere — a Makefile, a CI step, a pipeline.
        </p>
      </div>
    </Section>
  );
}

const RUNTIMES: [string, string, string][] = [
  ["Node.js", "nodejs.org", "macOS · Linux · x86-64 · arm64"],
  ["Python", "python-build-standalone", "macOS · Linux · x86-64 · arm64 · musl"],
  ["Go", "go.dev", "macOS · Linux · x86-64 · arm64 · musl"],
];

function WorksWith() {
  return (
    <Section
      id="works-with"
      index="06"
      title="Works with what you have"
      lede="Kiln installs runtimes from the places their maintainers publish them. It does not host, repackage, or fork anything."
    >
      <div className="grid gap-10 lg:grid-cols-[1fr_1fr] lg:gap-14 [&>*]:min-w-0">
        <div className="overflow-x-auto">
          <table className="w-full min-w-[22rem] text-left text-sm">
            <thead>
              <tr className="border-b border-rule text-xs uppercase tracking-wider text-ink-faint">
                <th className="py-2 font-normal">Runtime</th>
                <th className="py-2 font-normal">Source</th>
              </tr>
            </thead>
            <tbody>
              {RUNTIMES.map(([name, source, platforms]) => (
                <tr key={name} className="border-b border-rule/60">
                  <td className="py-3 pr-4 align-top font-medium">{name}</td>
                  <td className="py-3">
                    <span className="font-mono text-[13px] text-ink-muted">{source}</span>
                    <br />
                    <span className="font-mono text-xs text-ink-faint">{platforms}</span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          <p className="mt-5 text-[15px] leading-relaxed text-ink-muted">
            Adding a runtime is one file and one line in a registry. Go was added
            that way, and nothing else in the codebase changed.
          </p>
        </div>

        <div className="space-y-6">
          <Point title="Your tools keep working">
            Kiln puts the project's runtimes at the front of <C>PATH</C> and leaves
            the rest of it alone. Your <C>git</C>, <C>ssh</C>, and editor behave
            exactly as before. It shadows what a project pins; it does not take
            your machine away.
          </Point>
          <Point title="Offline once it has what it needs">
            A matching lockfile skips resolution entirely, and anything already in
            the store is never re-downloaded. A pinned project installs on a plane:
          </Point>
          <Terminal>
            <Cmd>kiln install --offline</Cmd>
            <Out tone="ok">{"  ✓ Node.js 22.14.0          already in the store"}</Out>
            <Out tone="ok">{"  ✓ Python 3.13.15           already in the store"}</Out>
            <Out tone="faint">{"  2 runtimes: 0 downloaded, 2 reused from the store"}</Out>
          </Terminal>
          <Point title="Safe in CI">
            <C>kiln install --locked</C> refuses to change the lockfile, so the
            environment CI installs is the one that was reviewed. The check is
            offline and structural — it fails before anything is fetched.
          </Point>
        </div>
      </div>
    </Section>
  );
}

const PIPELINE: [string, string][] = [
  ["kiln.toml", "what a human asked for"],
  ["Resolver", "match requirements to providers, offline"],
  ["Provider", "which version, which artifact"],
  ["Download", "hashed in flight, verified before unpacking"],
  ["Store", "content-addressed, promoted atomically"],
  ["Environment", "PATH composed, one child process"],
];

function Architecture() {
  return (
    <Section
      id="architecture"
      index="07"
      title="Architecture"
      lede="Eight crates, split by responsibility. The boundaries are the design."
    >
      <div className="grid gap-10 lg:grid-cols-[1fr_1fr] lg:gap-14 [&>*]:min-w-0">
        <ol className="space-y-0">
          {PIPELINE.map(([stage, detail], i) => (
            <li key={stage} className="flex gap-4 border-t border-rule py-4">
              <span className="mt-0.5 font-mono text-xs tabular-nums text-ink-faint">
                {String(i + 1).padStart(2, "0")}
              </span>
              <div>
                <p className="font-mono text-sm text-ink">{stage}</p>
                <p className="mt-1 text-sm text-ink-muted">{detail}</p>
              </div>
            </li>
          ))}
        </ol>

        <div className="space-y-6">
          <Point title="One crate can reach the network">
            Confining HTTP to a single crate turns "does this command need the
            network?" into a question you answer by reading the dependency graph,
            and gives <C>--offline</C> one place to be enforced.
          </Point>
          <Point title="The config layer knows no runtimes">
            <C>node = "22"</C> and <C>frobnicator = "22"</C> are equally well-formed
            to the parser. Deciding which names a real provider happens elsewhere,
            so adding a runtime never touches configuration.
          </Point>
          <Point title="Errors are a product surface">
            Every failure answers four questions: what happened, why, what Kiln
            expected, and what to do. Configuration errors point a caret at the
            exact token.
          </Point>
          <Point title="No unsafe code">
            Every crate is <C>#![forbid(unsafe_code)]</C>. 493 tests run offline;
            19 more exercise the real download path against real hosts.
          </Point>
        </div>
      </div>

      <div className="mt-10">
        <Terminal title="a mistyped version">
          <Out tone="bad">error: Invalid kiln.toml</Out>
          {"\n"}
          <Out tone="faint">{"  ┌─ kiln.toml:5:8"}</Out>
          <Out tone="faint">{"  │"}</Out>
          <Out>
            {"5 │ node = "}
            <span className="text-terminal-ink">"banana"</span>
          </Out>
          <Out tone="bad">{"  │        ^^^^^^^^ unsupported version requirement"}</Out>
          {"\n"}
          <Out tone="faint">{"  Expected:"}</Out>
          <Out tone="faint">{"    an exact version        22.14.0"}</Out>
          <Out tone="faint">{"    a major or minor pin    22          22.14"}</Out>
          <Out tone="faint">{"    a comparator range      >=22, <23"}</Out>
          <Out tone="faint">{"    a supported alias       lts         latest"}</Out>
        </Terminal>
      </div>
    </Section>
  );
}

const COMMANDS: [string, string][] = [
  ["kiln init", "Detect the project and propose a manifest"],
  ["kiln install", "Resolve, download, verify, install, write kiln.lock"],
  ["kiln lock", "Lock for one platform or all of them; --check for CI"],
  ["kiln shell", "A shell with the project's runtimes in front"],
  ["kiln run", "Run one command in the environment"],
  ["kiln list", "What is pinned, and whether it is installed"],
  ["kiln doctor", "Diagnose the project and the machine"],
  ["kiln cache", "Inspect the store; clean what is idle"],
];

function Commands() {
  return (
    <Section
      id="commands"
      index="08"
      title="Commands"
      lede={
        <>
          Nine commands. Anything not built yet fails loudly and says which
          release it belongs to — it never pretends to succeed.
        </>
      }
    >
      <div className="grid gap-x-10 gap-y-0 sm:grid-cols-2 [&>*]:min-w-0">
        {COMMANDS.map(([name, description]) => (
          <div key={name} className="border-t border-rule py-4">
            <p className="font-mono text-sm text-ember">{name}</p>
            <p className="mt-1 text-sm text-ink-muted">{description}</p>
          </div>
        ))}
      </div>

      <div className="mt-12 grid gap-8 lg:grid-cols-2 [&>*]:min-w-0">
        <div>
          <h3 className="font-mono text-xs uppercase tracking-wider text-ink-faint">
            Install from source
          </h3>
          <div className="mt-4">
            <Terminal>
              <Cmd>git clone {REPO.replace("https://github.com/", "git@github.com:")}.git</Cmd>
              <Cmd>cd Kiln</Cmd>
              <Cmd>cargo install --path crates/kiln-cli</Cmd>
            </Terminal>
          </div>
          <p className="mt-4 text-sm text-ink-muted">
            Rust 1.88 or newer. macOS and Linux, on x86-64 or arm64.
          </p>
        </div>

        <div>
          <h3 className="font-mono text-xs uppercase tracking-wider text-ink-faint">
            In continuous integration
          </h3>
          <div className="mt-4">
            <Terminal title=".github/workflows/ci.yml">
              <Out tone="faint">{"- run: "}</Out>
              <Out>{"    kiln lock --check"}</Out>
              <Out tone="faint">{"- run: "}</Out>
              <Out>{"    kiln install --locked"}</Out>
              <Out tone="faint">{"- run: "}</Out>
              <Out>{"    kiln run test"}</Out>
            </Terminal>
          </div>
          <p className="mt-4 text-sm text-ink-muted">
            <C>--check</C> fails if the lockfile drifted from the manifest.{" "}
            <C>--locked</C> refuses to install anything else.
          </p>
        </div>
      </div>
    </Section>
  );
}

const ROADMAP: [string, string, "done" | "next" | "planned"][] = [
  ["Manifest, validation, diagnostics", "kiln.toml and kiln init", "done"],
  ["Runtime management", "Node.js, Python and Go", "done"],
  ["Content-addressed cache", "install, share, collect", "done"],
  ["Environment activation", "kiln shell and kiln run", "done"],
  ["Deterministic locking", "cross-platform, --locked for CI", "done"],
  ["Cache verification", "needs a directory-hash scheme", "next"],
  ["Parallel downloads, mirrors, signatures", "hardening", "planned"],
  ["Services, OCI, Windows, IDE integration", "reach", "planned"],
];

function Status() {
  return (
    <Section
      id="status"
      index="09"
      title="Where Kiln is"
      lede="0.1.0. The loop works end to end: clone, install, shell, code. What remains is hardening and reach."
    >
      <ol>
        {ROADMAP.map(([title, detail, state]) => (
          <li key={title} className="flex items-baseline gap-4 border-t border-rule py-3.5">
            <span
              className={
                "mt-1 h-1.5 w-1.5 shrink-0 rounded-full " +
                (state === "done"
                  ? "bg-ember"
                  : state === "next"
                    ? "bg-ink-faint"
                    : "bg-rule")
              }
              aria-hidden
            />
            <div className="flex-1">
              <span className="text-[15px] text-ink">{title}</span>
              <span className="ml-2 text-sm text-ink-faint">{detail}</span>
            </div>
            <span className="font-mono text-xs text-ink-faint">
              {state === "done" ? "done" : state === "next" ? "next" : ""}
            </span>
          </li>
        ))}
      </ol>

      <div className="mt-10 rounded-lg border border-rule bg-paper-sunk p-6">
        <h3 className="font-semibold tracking-tight">Not planned</h3>
        <p className="mt-2 max-w-2xl text-[15px] leading-relaxed text-ink-muted">
          User accounts, a cloud dashboard, telemetry, remote execution, or
          replacing containers. Kiln reproduces environments. It is not trying to
          become a platform.
        </p>
      </div>
    </Section>
  );
}

function Footer() {
  return (
    <footer className="border-t border-rule py-12">
      <div className="mx-auto flex max-w-5xl flex-col gap-6 px-6 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex items-center gap-2.5">
          <Mark />
          <span className="text-sm text-ink-muted">
            Kiln — your development environment, committed.
          </span>
        </div>
        <div className="flex flex-wrap gap-x-6 gap-y-2 text-sm">
          <A href={REPO} external>
            GitHub
          </A>
          <A href={`${REPO}/blob/main/README.md`} external>
            Documentation
          </A>
          <A href={`${REPO}/blob/main/docs/architecture.md`} external>
            Architecture
          </A>
          <A href={`${REPO}/blob/main/SECURITY.md`} external>
            Security
          </A>
          <A href={`${REPO}/blob/main/CONTRIBUTING.md`} external>
            Contributing
          </A>
        </div>
      </div>
      <p className="mx-auto mt-8 max-w-5xl px-6 text-xs text-ink-faint">
        MIT licensed. No telemetry, and none planned.
      </p>
    </footer>
  );
}
