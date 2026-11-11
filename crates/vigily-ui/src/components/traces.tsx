import type { Payloads } from "@/lib/api";
import { compact, integer, money, moneyFull } from "@/lib/format";
import { cn } from "@/lib/utils";

/** A trace's or span's state as a word: plain when it went well, red with a square when it failed. */
export function TraceStatus({ status, className, iconOnly }: { status: "ok" | "error" | "unset"; className?: string; iconOnly?: boolean }) {
  const label = status === "error" ? "Error" : status === "ok" ? "OK" : "Unset";
  return (
    <span className={cn("inline-flex items-center gap-1.5 text-sm whitespace-nowrap", status === "error" ? "font-bold text-danger" : "text-muted-foreground", className)} title={iconOnly ? label : undefined}>
      {status === "error" && <span className="inline-block size-[7px] shrink-0 bg-danger" aria-hidden />}
      {iconOnly ? <span className="sr-only">{label}</span> : <span>{label}</span>}
    </span>
  );
}

/** A cost with its currency code; an em dash when no price is configured for the model. */
export function Cost({ value, currency, unpriced, className }: { value: number | null; currency: string; unpriced?: number; className?: string }) {
  if (value === null)
    return (
      <span className={cn("text-faint", className)} title="No price for this model">
        —<span className="sr-only">No price for this model</span>
      </span>
    );
  const note = unpriced ? `; ${unpriced} model ${unpriced === 1 ? "call has" : "calls have"} no price and ${unpriced === 1 ? "is" : "are"} not counted` : "";
  return (
    <span className={cn("whitespace-nowrap", className)} title={`${moneyFull(value, currency)}${note}`}>
      {money(value, currency)}
      {unpriced ? <span className="text-faint">+</span> : null}
    </span>
  );
}

export const PAYLOADS: Record<Payloads, string> = {
  full: "Full inputs and outputs",
  truncated: "Inputs and outputs truncated",
  metadata: "Metadata only",
};

/** Bytes as KiB, one decimal under 10. */
export function kib(bytes: number): string {
  const k = bytes / 1024;
  return `${k < 10 ? k.toFixed(1).replace(/\.0$/, "") : Math.round(k)}\u00a0KiB`;
}

export const TRACE_SNIPPET = `import vigily

@vigily.trace(kind="retrieval")
def search_docs(query: str) -> list[str]: ...

with vigily.trace("answer_question", run=run) as root:
    root.set_input(question)
    docs = search_docs(question)
    with vigily.trace("chat", kind="llm", model="example-large", provider="example") as llm:
        llm.set_input([{"role": "user", "content": question}])
        reply = my_client.complete(question, docs)
        llm.set_output(reply.text)
        llm.set_usage(input_tokens=812, output_tokens=95)
    root.set_output(reply.text)`;

/** Input and output tokens as "1,880 / 151", for table cells. */
export function Tokens({ input, output, cached, className }: { input: number; output: number; cached?: number; className?: string }) {
  if (!input && !output) return <span className={cn("text-faint", className)}>–</span>;
  return (
    <span className={cn("whitespace-nowrap nums", className)} title={`${integer(input)} input tokens${cached ? `, ${integer(cached)} of them cached` : ""}; ${integer(output)} output tokens`}>
      {integer(input)} <span className="text-faint">/</span> {integer(output)}
      {cached ? <span className="ml-1.5 text-faint">{compact(cached)} cached</span> : null}
    </span>
  );
}
