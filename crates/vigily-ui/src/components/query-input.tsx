import { CircleHelp, Search, X } from "lucide-react";
import { forwardRef, useEffect, useId, useImperativeHandle, useMemo, useRef, useState } from "react";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { type Meta, isSystem } from "@/lib/api";
import { cn } from "@/lib/utils";

const EXAMPLES: [string, string][] = [
  ["resnet", "a word in the name, experiment, tags or notes"],
  ["status:failed", "running, finished, failed or aborted"],
  ["exp:cifar", "the experiment contains “cifar”"],
  ["lr<0.01", "a parameter, by its last key or full key"],
  ["optimizer.name=adamw", "text compares without case"],
  ["loss[val]<0.3", "a metric’s last value in a context"],
  ["loss[val].min<0.25", ".min, .max, .last or .count"],
  ["duration>10m", "durations in s, m, h or d; also age"],
  ["tag:baseline -tag:old", "- or not negates a term"],
  ["kind:eval target.model=clf-v2", "evaluation runs of one target"],
  ["exp:mnist or exp:cifar", "either group matches"],
];

const FIELDS = ["status:", "exp:", "tag:", "name:", "notes:", "kind:", "target:", "duration>", "age<", "archived=true"];

export interface QueryInputHandle {
  focus: () => void;
}

interface Props {
  value: string;
  onChange: (q: string) => void;
  meta?: Meta;
  error?: { message: string; position?: number } | null;
  placeholder?: string;
  className?: string;
  label?: string;
}

/** The current word being typed, for completion: its start and text. */
function wordAt(text: string, caret: number): { start: number; word: string } {
  let start = caret;
  while (start > 0 && !/\s/.test(text[start - 1])) start--;
  return { start, word: text.slice(start, caret) };
}

/** A run query field: completes field, parameter and metric names; explains the syntax; shows errors at their position. */
export const QueryInput = forwardRef<QueryInputHandle, Props>(function QueryInput({ value, onChange, meta, error, placeholder, className, label = "Filter runs" }, ref) {
  const input = useRef<HTMLInputElement>(null);
  const [text, setText] = useState(value);
  const [caret, setCaret] = useState(0);
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const listId = useId();
  const errId = useId();
  useImperativeHandle(ref, () => ({ focus: () => input.current?.focus() }), []);

  // Follow outside changes (navigation, a cleared filter) without clobbering typing.
  const [prev, setPrev] = useState(value);
  if (value !== prev) {
    setPrev(value);
    if (value !== text) setText(value);
  }

  useEffect(() => {
    if (text === value) return;
    const t = setTimeout(() => onChange(text.trim()), 220);
    return () => clearTimeout(t);
  }, [text, value, onChange]);

  const keys = useMemo(() => {
    if (!meta) return FIELDS;
    const params = meta.params.filter((p) => !isSystem(p.key)).map((p) => p.key);
    const metrics = meta.metrics.filter((m) => !isSystem(m.name)).map((m) => m.key);
    return [...FIELDS, ...params, ...metrics];
  }, [meta]);

  const { start, word } = wordAt(text, caret);
  const bare = word.replace(/^-/, "");
  const suggestions = useMemo(() => {
    if (bare.length < 1 || /[<>=:!]/.test(bare)) return [];
    const w = bare.toLowerCase();
    const starts = keys.filter((k) => k.toLowerCase().startsWith(w) || k.toLowerCase().split(".").pop()!.startsWith(w));
    const contains = keys.filter((k) => !starts.includes(k) && k.toLowerCase().includes(w));
    return [...starts, ...contains].filter((k) => k !== bare).slice(0, 8);
  }, [bare, keys]);
  const showList = open && suggestions.length > 0;

  const accept = (k: string) => {
    const neg = word.startsWith("-") ? "-" : "";
    const next = text.slice(0, start) + neg + k + text.slice(caret);
    const pos = start + neg.length + k.length;
    setText(next);
    setOpen(false);
    requestAnimationFrame(() => {
      input.current?.setSelectionRange(pos, pos);
      setCaret(pos);
    });
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (showList && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
      e.preventDefault();
      setActive((a) => (a + (e.key === "ArrowDown" ? 1 : suggestions.length - 1)) % suggestions.length);
    } else if (showList && (e.key === "Tab" || e.key === "Enter")) {
      e.preventDefault();
      accept(suggestions[Math.min(active, suggestions.length - 1)]);
    } else if (e.key === "Enter") {
      onChange(text.trim());
    } else if (e.key === "Escape") {
      if (showList) setOpen(false);
      else input.current?.blur();
    }
  };

  return (
    <div className={cn("relative min-w-0", className)}>
      <div className={cn("flex h-9 items-center border bg-card pointer-coarse:h-11", error ? "border-danger" : "border-input", "focus-within:outline-2 focus-within:outline-offset-2 focus-within:outline-ring")}>
        <Search className="ml-2.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
        <input
          ref={input}
          role="combobox"
          aria-label={label}
          aria-expanded={showList}
          aria-controls={listId}
          aria-autocomplete="list"
          aria-activedescendant={showList ? `${listId}-${active}` : undefined}
          aria-invalid={!!error}
          aria-describedby={error ? errId : undefined}
          spellCheck={false}
          autoCapitalize="off"
          autoComplete="off"
          value={text}
          placeholder={placeholder ?? "Filter: lr<0.01 status:finished loss[val]<0.3"}
          onChange={(e) => {
            setText(e.target.value);
            setCaret(e.target.selectionStart ?? e.target.value.length);
            setOpen(true);
            setActive(0);
          }}
          onSelect={(e) => setCaret((e.target as HTMLInputElement).selectionStart ?? 0)}
          onKeyDown={onKeyDown}
          onBlur={() => setTimeout(() => setOpen(false), 120)}
          className="h-full min-w-0 flex-1 bg-transparent px-2 font-mono text-sm outline-none placeholder:font-sans placeholder:text-muted-foreground"
        />
        {text && (
          <button
            type="button"
            aria-label="Clear the filter"
            onClick={() => {
              setText("");
              onChange("");
              input.current?.focus();
            }}
            className="inline-flex size-7 shrink-0 items-center justify-center rounded-sm text-muted-foreground hover:text-foreground pointer-coarse:size-11"
          >
            <X className="size-4" />
          </button>
        )}
        <Popover>
          <PopoverTrigger asChild>
            <button type="button" aria-label="How to filter runs" className="mr-0.5 inline-flex size-7 shrink-0 items-center justify-center rounded-sm text-muted-foreground hover:text-foreground pointer-coarse:size-11">
              <CircleHelp className="size-4" />
            </button>
          </PopoverTrigger>
          <PopoverContent align="end" className="w-[min(30rem,calc(100vw-2rem))] p-3">
            <h3 className="text-sm font-semibold">Filter runs</h3>
            <p className="mt-0.5 text-sm text-muted-foreground">Terms separated by spaces must all match. Click an example to use it.</p>
            <ul className="mt-2 flex flex-col">
              {EXAMPLES.map(([q, help]) => (
                <li key={q}>
                  <button
                    type="button"
                    onClick={() => {
                      setText(q);
                      onChange(q);
                    }}
                    className="grid w-full grid-cols-[minmax(9rem,auto)_1fr] gap-3 rounded-sm px-1.5 py-1 text-left hover:bg-muted"
                  >
                    <code className="text-sm text-foreground">{q}</code>
                    <span className="text-sm text-muted-foreground">{help}</span>
                  </button>
                </li>
              ))}
            </ul>
          </PopoverContent>
        </Popover>
      </div>
      {showList && (
        <ul id={listId} role="listbox" aria-label="Suggestions" className="absolute left-0 top-full z-40 mt-1 w-full max-w-md overflow-hidden rounded-md bg-popover py-1 shadow-float">
          {suggestions.map((k, i) => (
            <li
              key={k}
              id={`${listId}-${i}`}
              role="option"
              aria-selected={i === active}
              onMouseDown={(e) => {
                e.preventDefault();
                accept(k);
              }}
              className={cn("cursor-default truncate px-2.5 py-1 font-mono text-sm", i === active && "bg-muted")}
            >
              {k}
            </li>
          ))}
        </ul>
      )}
      {error && (
        <p id={errId} role="alert" className="mt-1 text-sm text-danger">
          {error.message.split("`").map((part, i) => (i % 2 ? <code key={i} className="rounded-sm bg-muted px-1 text-foreground">{part}</code> : part))}
          {error.position !== undefined && text && (
            <>
              {" "}
              at <code className="rounded-sm bg-muted px-1 text-foreground">{text.slice(Math.max(0, error.position - 8), error.position + 8) || "the end"}</code>
            </>
          )}
        </p>
      )}
    </div>
  );
});
