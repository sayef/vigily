import { cn } from "@/lib/utils";

/**
 * The page header: a compact title, one line of facts and the actions on the right, closed by an
 * ink rule. Facts are short phrases; put the figure in <b> to make it stand out.
 */
export function PageHeader({ title, children, actions, className }: { title: React.ReactNode; children?: React.ReactNode; actions?: React.ReactNode; className?: string }) {
  return (
    <div className={cn("flex flex-wrap items-end gap-x-4 gap-y-3 border-b-[1.5px] border-rule pb-4", className)}>
      <div className="min-w-0 flex-1 basis-64">
        <h1 className="break-words text-xl font-bold tracking-[-0.02em]">{title}</h1>
        {children && <div className="mt-1.5 flex flex-wrap gap-x-[18px] gap-y-0.5 text-sm text-faint nums [&_b]:font-semibold [&_b]:text-foreground">{children}</div>}
      </div>
      {actions && <div className="flex flex-wrap gap-2">{actions}</div>}
    </div>
  );
}

/** One finding under the page header: a coloured tag and a sentence. `tone` bad is red, good ink blue. */
export function Insight({ tag, tone = "good", children, action }: { tag: string; tone?: "good" | "bad" | "plain"; children: React.ReactNode; action?: React.ReactNode }) {
  return (
    <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1 border-b py-3 text-base" role={tone === "bad" ? "status" : undefined}>
      <span
        className={cn(
          "inline-flex shrink-0 items-center gap-2 text-[11px] font-bold uppercase tracking-[0.12em]",
          tone === "bad" ? "text-danger" : tone === "good" ? "text-brand" : "text-faint",
        )}
      >
        {tone === "bad" && <span className="inline-block size-2 bg-danger" aria-hidden />}
        {tag}
      </span>
      <span className="min-w-0 flex-1 basis-72">{children}</span>
      {action && <span className="shrink-0 font-bold underline underline-offset-4 [&_a]:inline-flex [&_a]:min-h-6 [&_a]:items-center">{action}</span>}
    </div>
  );
}

/** A row of large figures separated by hairlines. Two per row on phones. */
export function Figures({ children, label }: { children: React.ReactNode; label?: string }) {
  return (
    <section aria-label={label ?? "Summary"} className="grid grid-cols-2 border-b lg:grid-cols-4 [&>*]:border-border max-lg:[&>*:nth-child(-n+2)]:border-b lg:[&>*+*]:border-l max-lg:[&>*:nth-child(2n)]:border-l">
      {children}
    </section>
  );
}

export function Figure({ label, value, unit, note, tone }: { label: string; value: React.ReactNode; unit?: React.ReactNode; note?: React.ReactNode; tone?: "bad" | "good" }) {
  return (
    <div className="min-w-0 py-4 pr-4 [&:not(:first-child)]:pl-4 lg:[&:not(:first-child)]:pl-5">
      <p className="truncate text-[11px] font-bold uppercase tracking-[0.12em] text-faint">{label}</p>
      <p className={cn("mt-2 mb-1 truncate text-[38px] font-light leading-none tracking-[-0.01em] nums", tone === "bad" && "text-danger", tone === "good" && "text-brand")}>
        {value}
        {unit && <small className="ml-1.5 text-base font-normal tracking-normal text-faint">{unit}</small>}
      </p>
      {note && <p className="truncate text-sm text-muted-foreground [&_b]:font-bold">{note}</p>}
    </div>
  );
}

/** A section heading over an ink rule, with optional tools (view switches, links) on the right. */
export function SectionHead({ id, title, tools, className }: { id?: string; title: React.ReactNode; tools?: React.ReactNode; className?: string }) {
  return (
    <div className={cn("flex flex-wrap items-baseline gap-x-4 gap-y-2 border-b-[1.5px] border-rule pb-2.5 pt-7", className)}>
      <h2 id={id} className="text-[17px] font-bold tracking-[-0.01em]">
        {title}
      </h2>
      {tools && <div className="ml-auto flex flex-wrap items-baseline gap-x-[18px] gap-y-1 text-sm text-muted-foreground">{tools}</div>}
    </div>
  );
}

/** A text switch in a section's tools: the chosen one is bold and underlined. */
export function ToolSwitch<T extends string>({ options, value, onChange, label }: { options: [T, React.ReactNode][]; value: T; onChange: (v: T) => void; label: string }) {
  return (
    <span role="radiogroup" aria-label={label} className="flex flex-wrap gap-x-[18px]">
      {options.map(([k, l]) => (
        <button
          key={k}
          type="button"
          role="radio"
          aria-checked={value === k}
          onClick={() => onChange(k)}
          className={cn(
            "min-h-6 min-w-6 text-sm underline-offset-[5px] pointer-coarse:min-h-11",
            value === k ? "font-bold text-foreground underline decoration-2" : "text-muted-foreground hover:text-foreground",
          )}
        >
          {l}
        </button>
      ))}
    </span>
  );
}

/** A note in the margin column beside a figure or table. */
export function MarginNote({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="pb-3 text-[12.5px] leading-[1.55] text-faint">
      <b className="mb-1 block text-sm text-foreground">{title}</b>
      {children}
    </div>
  );
}

/** A figure caption under a chart: "Fig. 1" in ink, then the description; extra text on the right. */
export function Caption({ n, children, right }: { n: number; children: React.ReactNode; right?: React.ReactNode }) {
  return (
    <figcaption className="flex flex-wrap gap-x-6 gap-y-1 border-t pt-2.5 text-[12.5px] text-faint nums">
      <span>
        <b className="text-foreground">Fig. {n}</b> {children}
      </span>
      {right && <span className="ml-auto">{right}</span>}
    </figcaption>
  );
}
