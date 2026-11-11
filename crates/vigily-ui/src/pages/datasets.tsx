import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { Database, Search } from "lucide-react";
import { useState } from "react";
import { Link, useParams, useSearchParams } from "react-router";
import { PageHeader, jsonText } from "@/components/evals";
import { ErrorState } from "@/components/states";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { Tooltip } from "@/components/ui/tooltip";
import { evalApi } from "@/lib/api";
import { ago, integer, utc } from "@/lib/format";
import { useCrumbs } from "@/lib/crumbs";
import { useDebounced, useTitle } from "@/lib/hooks";
import { cn } from "@/lib/utils";

const SNIPPET = `from vigily import Dataset

ds = Dataset.create("support-intents", [
    {"input": "Where is my parcel 1037?", "expected": "shipping", "metadata": {"lang": "en"}},
    {"input": "Rechnung 1044 wurde doppelt abgebucht", "expected": "billing", "metadata": {"lang": "de"}},
])
ds = Dataset.from_jsonl("support-intents", "intents.jsonl")   # or from_csv(...)`;

export function DatasetsPage() {
  useTitle("Datasets");
  const q = useQuery({ queryKey: ["datasets"], queryFn: ({ signal }) => evalApi.datasets(signal) });
  if (q.isError) return <ErrorState error={q.error as Error} onRetry={() => q.refetch()} />;
  const list = q.data ?? [];
  return (
    <div className="px-4 pb-16 pt-5 md:px-6">
      <PageHeader title="Datasets">
        {q.data && list.length > 0
          ? `${list.length} ${list.length === 1 ? "dataset" : "datasets"}. Every change is a new version, and every evaluation records the version it used.`
          : " "}
      </PageHeader>
      {q.data && list.length === 0 ? (
        <div className="mt-6 max-w-2xl">
          <h2 className="text-[17px] font-bold tracking-[-0.01em]">No datasets yet</h2>
          <p className="mt-1 text-base text-muted-foreground">
            A dataset holds the items an evaluation scores: an input, the expected output and metadata. Create one from Python, or import JSON Lines or CSV.
          </p>
          <pre className="mt-3 overflow-x-auto p-4 font-mono text-sm leading-relaxed">
            <code>{SNIPPET}</code>
          </pre>
        </div>
      ) : (
        <div className="relative mt-4 max-w-6xl overflow-x-auto">
          <table className="w-full min-w-[40rem] text-sm">
            <thead>
              <tr className="text-left text-muted-foreground">
                <th scope="col" className="h-10 border-b px-3 font-medium">
                  Dataset
                </th>
                <th scope="col" className="h-10 border-b px-3 text-right font-medium">
                  Items
                </th>
                <th scope="col" className="h-10 border-b px-3 text-right font-medium">
                  Version
                </th>
                <th scope="col" className="h-10 border-b px-3 font-medium">
                  Changed
                </th>
                <th scope="col" className="h-10 border-b px-3 font-medium">
                  Description
                </th>
              </tr>
            </thead>
            <tbody>
              {!q.data &&
                Array.from({ length: 3 }, (_, i) => (
                  <tr key={i}>
                    {Array.from({ length: 5 }, (_, j) => (
                      <td key={j} className="h-11 border-b px-3">
                        <Skeleton className="h-3 w-24" />
                      </td>
                    ))}
                  </tr>
                ))}
              {list.map((d) => (
                <tr key={d.name} className="hover:bg-muted">
                  <th scope="row" className="h-11 border-b px-3 text-left font-medium">
                    <Link to={`/datasets/${encodeURIComponent(d.name)}`} className="inline-flex min-h-8 items-center gap-2 hover:underline">
                      <Database className="size-4 text-muted-foreground" aria-hidden />
                      {d.name}
                    </Link>
                  </th>
                  <td className="h-11 border-b px-3 text-right nums">{integer(d.items)}</td>
                  <td className="h-11 border-b px-3 text-right nums">v{d.latest}</td>
                  <td className="h-11 border-b px-3 text-muted-foreground">
                    <Tooltip content={utc(d.updated_at)}>
                      <span>{ago(d.updated_at)}</span>
                    </Tooltip>
                  </td>
                  <td className="h-11 max-w-md truncate border-b px-3 text-muted-foreground" title={d.description ?? undefined}>
                    {d.description ?? "–"}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

export function DatasetPage() {
  const { name = "" } = useParams();
  useTitle(name);
  useCrumbs([{ label: "Datasets", to: "/datasets" }, { label: name, mono: true }]);
  const [sp, setSp] = useSearchParams();
  const version = Number(sp.get("version")) || null;
  const [text, setText] = useState(sp.get("q") ?? "");
  const q = useDebounced(text.trim(), 250);
  const data = useQuery({
    queryKey: ["dataset", name, version, q],
    queryFn: ({ signal }) => evalApi.dataset(name, { version, q }, signal),
    placeholderData: keepPreviousData,
  });
  if (data.isError) return <ErrorState error={data.error as Error} onRetry={() => data.refetch()} />;
  const d = data.data;
  const setVersion = (v: number | null) =>
    setSp(
      (p) => {
        if (v === null || v === d?.info.latest) p.delete("version");
        else p.set("version", String(v));
        return p;
      },
      { replace: true },
    );

  return (
    <div className="px-4 pb-16 pt-5 md:px-6">
      <PageHeader title={<span className="font-mono">{name}</span>}>
        {d ? (
          <>
            {d.info.description && <span className="text-foreground">{d.info.description}. </span>}
            Version {d.version.version} of {d.info.latest}, {integer(d.version.items)} items, digest <code className="text-foreground">{d.version.digest}</code>.
          </>
        ) : (
          " "
        )}
      </PageHeader>

      <div className="mt-5 grid gap-6 xl:grid-cols-[minmax(0,1fr)_20rem]">
        <section aria-labelledby="items-h" className="min-w-0">
          <div className="mb-2 flex flex-wrap items-center gap-3">
            <h2 id="items-h" className="text-[17px] font-bold tracking-[-0.01em]">
              Items
            </h2>
            <div className="relative ml-auto w-full max-w-xs">
              <Search className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" aria-hidden />
              <Input value={text} onChange={(e) => setText(e.target.value)} placeholder="Find an id, input or expected output…" aria-label="Find items" className="pl-8" />
            </div>
          </div>
          <div className={cn("relative overflow-x-auto", data.isFetching && data.isPlaceholderData && "opacity-80")}>
            <table className="w-full min-w-[40rem] text-sm">
              <thead>
                <tr className="text-left text-muted-foreground">
                  <th scope="col" className="h-9 w-40 border-b px-3 font-medium">
                    Id
                  </th>
                  <th scope="col" className="h-9 border-b px-3 font-medium">
                    Input
                  </th>
                  <th scope="col" className="h-9 border-b px-3 font-medium">
                    Expected
                  </th>
                  <th scope="col" className="h-9 border-b px-3 font-medium">
                    Metadata
                  </th>
                </tr>
              </thead>
              <tbody>
                {!d &&
                  Array.from({ length: 10 }, (_, i) => (
                    <tr key={i}>
                      {Array.from({ length: 4 }, (_, j) => (
                        <td key={j} className="h-8 border-b px-3">
                          <Skeleton className="h-3 w-28" />
                        </td>
                      ))}
                    </tr>
                  ))}
                {d?.items.map((it) => (
                  <tr key={it.id} className="align-top hover:bg-muted">
                    <td className="border-b px-3 py-1.5 font-mono text-[12.5px] text-muted-foreground">{it.id}</td>
                    <td className="max-w-md border-b px-3 py-1.5">
                      <span className="line-clamp-2 break-words">{jsonText(it.input)}</span>
                    </td>
                    <td className="max-w-xs border-b px-3 py-1.5">
                      <span className="line-clamp-2 break-words">{jsonText(it.expected) || "–"}</span>
                    </td>
                    <td className="border-b px-3 py-1.5">
                      <span className="flex flex-wrap gap-1">
                        {Object.entries(it.metadata ?? {}).map(([k, v]) => (
                          <span key={k} className="rounded-sm bg-muted px-1.5 text-xs leading-5">
                            <span className="text-muted-foreground">{k}</span> {jsonText(v)}
                          </span>
                        ))}
                      </span>
                    </td>
                  </tr>
                ))}
                {d && d.items.length === 0 && (
                  <tr>
                    <td colSpan={4} className="px-3 py-6 text-center text-muted-foreground">
                      No item matches “{text}”.
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
          {d && d.total > d.items.length && <p className="mt-2 text-sm text-muted-foreground">Showing {d.items.length} of {integer(d.total)} items. Narrow the search to see others.</p>}
        </section>

        <aside className="flex flex-col gap-6">
          <section aria-labelledby="versions-h">
            <h2 id="versions-h" className="mb-3 mt-6 border-b-[1.5px] border-rule pb-2.5 text-[17px] font-bold tracking-[-0.01em]">
              Versions
            </h2>
            <ol className="flex flex-col gap-1">
              {d?.versions.map((v) => {
                const current = v.version === d.version.version;
                return (
                  <li key={v.version}>
                    <button
                      type="button"
                      onClick={() => setVersion(v.version)}
                      aria-current={current ? "true" : undefined}
                      className={cn("w-full rounded-md px-3 py-2 text-left text-sm hover:bg-muted", current && "bg-muted hover:bg-muted")}
                    >
                      <span className="flex items-baseline justify-between gap-2">
                        <span className="font-semibold nums">v{v.version}</span>
                        <Tooltip content={utc(v.created_at)}>
                          <span className="text-xs text-muted-foreground">{ago(v.created_at)}</span>
                        </Tooltip>
                      </span>
                      {v.note && <span className="mt-0.5 block text-muted-foreground">{v.note}</span>}
                      <span className="mt-0.5 block text-xs text-muted-foreground nums">
                        {integer(v.items)} items
                        {v.changes.added > 0 && `, ${v.changes.added} added`}
                        {v.changes.updated > 0 && `, ${v.changes.updated} changed`}
                        {v.changes.removed > 0 && `, ${v.changes.removed} removed`}
                        {v.source && `, from ${v.source}`}
                      </span>
                    </button>
                  </li>
                );
              })}
            </ol>
          </section>
          {d && d.runs.length > 0 && (
            <section aria-labelledby="used-h">
              <h2 id="used-h" className="mb-3 mt-6 border-b-[1.5px] border-rule pb-2.5 text-[17px] font-bold tracking-[-0.01em]">
                Evaluated in
              </h2>
              <ul className="flex flex-col">
                {d.runs.slice(0, 12).map((r) => (
                  <li key={r.id}>
                    <Link to={`/runs/${r.id}?tab=items`} className="flex min-h-8 items-baseline gap-2 rounded-sm px-1 text-sm hover:bg-muted">
                      <span className="min-w-0 flex-1 truncate">{r.name}</span>
                      <span className="shrink-0 text-xs text-muted-foreground nums">v{r.version}</span>
                    </Link>
                  </li>
                ))}
              </ul>
              {d.runs.length > 12 && <p className="mt-1 text-xs text-muted-foreground">and {d.runs.length - 12} more.</p>}
            </section>
          )}
        </aside>
      </div>
    </div>
  );
}
