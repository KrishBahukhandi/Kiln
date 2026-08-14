import type { ReactNode } from "react";

/**
 * The pieces the page is assembled from.
 *
 * Kept deliberately few. A marketing site for an infrastructure tool needs
 * about six primitives, and every extra one is another chance for two sections
 * to disagree about what a heading looks like.
 */

/** A page section with an editorial number-and-rule header. */
export function Section({
  id,
  index,
  title,
  lede,
  children,
}: {
  id: string;
  index: string;
  title: string;
  lede?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <section id={id} className="scroll-mt-20 border-t border-rule py-16 sm:py-24">
      <div className="mx-auto max-w-5xl px-6">
        <div className="flex items-baseline gap-4">
          <span className="font-mono text-xs tabular-nums text-ember">{index}</span>
          <h2 className="text-2xl font-semibold tracking-tight sm:text-3xl">{title}</h2>
        </div>
        {lede && (
          <p className="mt-5 max-w-2xl text-lg leading-relaxed text-ink-muted">{lede}</p>
        )}
        {children && <div className="mt-10">{children}</div>}
      </div>
    </section>
  );
}

/**
 * A terminal specimen.
 *
 * The most credible thing this site can show is what the tool actually prints,
 * so these hold real output rather than a prettified imitation of it.
 */
export function Terminal({
  title,
  children,
}: {
  title?: string;
  children: ReactNode;
}) {
  return (
    <div className="overflow-hidden rounded-lg border border-terminal-rule bg-terminal shadow-sm">
      {title && (
        <div className="flex items-center gap-2 border-b border-terminal-rule px-4 py-2.5">
          <span className="h-2.5 w-2.5 rounded-full bg-terminal-rule" aria-hidden />
          <span className="ml-2 font-mono text-xs text-terminal-faint">{title}</span>
        </div>
      )}
      <pre className="overflow-x-auto px-4 py-4 font-mono text-[13px] leading-relaxed text-terminal-ink sm:px-5">
        {children}
      </pre>
    </div>
  );
}

/** A shell prompt line inside a terminal. */
export function Cmd({ children }: { children: ReactNode }) {
  return (
    <span>
      <span className="select-none text-ember-bright">$ </span>
      <span className="text-terminal-ink">{children}</span>
      {"\n"}
    </span>
  );
}

/** De-emphasised terminal output. */
export function Out({
  children,
  tone = "plain",
}: {
  children: ReactNode;
  tone?: "plain" | "faint" | "ok" | "bad" | "accent";
}) {
  const tones = {
    plain: "text-terminal-ink",
    faint: "text-terminal-faint",
    ok: "text-terminal-green",
    bad: "text-terminal-red",
    accent: "text-terminal-cyan",
  } as const;
  return (
    <span className={tones[tone]}>
      {children}
      {"\n"}
    </span>
  );
}

/** A file specimen: a named block of configuration. */
export function FileBlock({
  name,
  children,
}: {
  name: string;
  children: ReactNode;
}) {
  return (
    <div className="overflow-hidden rounded-lg border border-rule bg-paper-sunk">
      <div className="border-b border-rule px-4 py-2.5">
        <span className="font-mono text-xs text-ink-faint">{name}</span>
      </div>
      <pre className="overflow-x-auto px-4 py-4 font-mono text-[13px] leading-relaxed sm:px-5">
        {children}
      </pre>
    </div>
  );
}

/** Syntax colour for the small amount of TOML on the page. */
export function Key({ children }: { children: ReactNode }) {
  return <span className="text-ember">{children}</span>;
}
export function Str({ children }: { children: ReactNode }) {
  return <span className="text-ink">{children}</span>;
}
export function Comment({ children }: { children: ReactNode }) {
  return <span className="text-ink-faint">{children}</span>;
}

/** A claim with its justification. The justification is the point. */
export function Point({
  title,
  children,
}: {
  title: string;
  children: ReactNode;
}) {
  return (
    <div className="border-t border-rule pt-5">
      <h3 className="font-semibold tracking-tight">{title}</h3>
      <p className="mt-2 text-[15px] leading-relaxed text-ink-muted">{children}</p>
    </div>
  );
}

/** Inline code in prose. */
export function C({ children }: { children: ReactNode }) {
  return (
    <code className="rounded bg-paper-sunk px-1.5 py-0.5 font-mono text-[0.9em] text-ink">
      {children}
    </code>
  );
}

/** A link that looks like a link without shouting. */
export function A({
  href,
  children,
  external,
}: {
  href: string;
  children: ReactNode;
  external?: boolean;
}) {
  return (
    <a
      href={href}
      {...(external ? { target: "_blank", rel: "noreferrer noopener" } : {})}
      className="text-ember underline decoration-ember/30 underline-offset-4 transition-colors hover:decoration-ember"
    >
      {children}
    </a>
  );
}
