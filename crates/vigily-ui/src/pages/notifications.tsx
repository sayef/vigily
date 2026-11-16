import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowDown, ArrowUp, Plus, Send, Trash2 } from "lucide-react";
import { useState } from "react";
import { Link } from "react-router";
import { toast } from "sonner";
import { DeliveryLine, KindIcon, KINDS, SecretStateText, SettingsNav, sentence, TestOutcome, useCanAdmin } from "@/components/notifications";
import { Insight, PageHeader, SectionHead } from "@/components/page";
import { ErrorState } from "@/components/states";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { Tooltip } from "@/components/ui/tooltip";
import { ApiError, type AlertKind, type Channel, notifyApi, type Rule, type Rules, SECRET_FIELDS, type Severity, type TestResult } from "@/lib/api";
import { useCrumbs } from "@/lib/crumbs";
import { ago, utc } from "@/lib/format";
import { useTitle } from "@/lib/hooks";
import { cn } from "@/lib/utils";

export const ALERT_KINDS: [AlertKind, string][] = [
  ["behaviour_drift", "Behaviour drift"],
  ["data_drift", "Input drift"],
  ["schedule_failed", "Schedule failed"],
];
const SEVERITIES: [Severity, string][] = [
  ["warning", "Warning"],
  ["critical", "Critical"],
];

export function useChannels() {
  return useQuery({ queryKey: ["channels"], queryFn: ({ signal }) => notifyApi.channels(signal), refetchInterval: 30_000 });
}

export function useSecrets() {
  return useQuery({ queryKey: ["secrets"], queryFn: ({ signal }) => notifyApi.secrets(signal), staleTime: 10_000 });
}

/** Settings → Notifications: the channels alerts are sent to and the rules that route them. */
export function NotificationsPage() {
  useTitle("Notifications");
  useCrumbs([{ label: "Settings", to: "/settings" }, { label: "Notifications" }]);
  const { admin, known } = useCanAdmin();
  const channels = useChannels();
  const secrets = useSecrets();
  const rules = useQuery({ queryKey: ["rules"], queryFn: ({ signal }) => notifyApi.rules(signal) });
  if (channels.isError) return <ErrorState error={channels.error as Error} onRetry={() => channels.refetch()} />;
  const list = channels.data?.channels;
  const s = secrets.data;
  const undecryptable = list?.filter((c) => Object.values(c.secrets).some((x) => x.undecryptable)) ?? [];
  return (
    <div className="px-4 pb-16 pt-5 md:px-6">
      <SettingsNav />
      <PageHeader
        title="Notifications"
        actions={
          admin &&
          !!list?.length && (
            <Button asChild variant="brand">
              <Link to="/settings/notifications/channels/new">
                <Plus /> Add a channel
              </Link>
            </Button>
          )
        }
      >
        {list ? (
          <>
            <span>
              <b>{list.length}</b> {list.length === 1 ? "channel" : "channels"}
            </span>
            <span>
              <b>{rules.data?.rules.length ?? "–"}</b> routing {rules.data?.rules.length === 1 ? "rule" : "rules"}
            </span>
            {s && <span>{s.key === "env" ? "Secrets encrypted with VIGILY_SECRET_KEY" : s.key === "file" ? "Secrets encrypted with this engine's generated key" : "No encryption key"}</span>}
          </>
        ) : (
          " "
        )}
      </PageHeader>
      {s && !s.can_store && (
        <Insight tag="No key" tone="plain">
          {s.message ? sentence(s.message) : "Storing secret values needs VIGILY_SECRET_KEY; reference an environment variable instead (env:NAME)."}
        </Insight>
      )}
      {undecryptable.length > 0 && (
        <Insight tag="Undecryptable" tone="bad">
          {undecryptable.map((c) => c.name).join(", ")} {undecryptable.length === 1 ? "has a secret" : "have secrets"} no key decrypts. Store {undecryptable.length === 1 ? "it" : "them"} again, or
          set VIGILY_SECRET_KEY_PREVIOUS to the key {undecryptable.length === 1 ? "it was" : "they were"} stored with.
        </Insight>
      )}
      {known && !admin && (
        <Insight tag="Read only" tone="plain">
          Your token can read these settings. Adding, changing and testing channels or rules needs a token with the admin scope.
        </Insight>
      )}

      <SectionHead id="channels-h" title="Channels" tools={list && list.length > 0 && <span>Alerts are delivered once per channel, with retries.</span>} />
      {!list ? (
        <div className="mt-3 flex flex-col gap-2">
          {Array.from({ length: 3 }, (_, i) => (
            <Skeleton key={i} className="h-10" />
          ))}
        </div>
      ) : list.length === 0 ? (
        <EmptyChannels admin={admin} />
      ) : (
        <ChannelList channels={list} admin={admin} />
      )}

      <RulesSection channels={list ?? []} admin={admin} rules={rules.data} error={rules.error as Error | null} onRetry={() => rules.refetch()} />
    </div>
  );
}

function EmptyChannels({ admin }: { admin: boolean }) {
  return (
    <div className="max-w-2xl py-6">
      <p className="font-semibold">No channels yet.</p>
      <p className="mt-1 text-base text-muted-foreground">
        A channel is where alerts are sent: a Slack, Microsoft Teams or Discord webhook, any HTTPS endpoint (signed JSON), or e-mail over SMTP. Alerts go to the channels marked
        default, or to the ones your routing rules name.
      </p>
      {admin ? (
        <Button asChild variant="brand" className="mt-4">
          <Link to="/settings/notifications/channels/new">
            <Plus /> Add the first channel
          </Link>
        </Button>
      ) : (
        <p className="mt-3 text-sm text-muted-foreground">
          An admin can add one here or with <code className="rounded-sm bg-muted px-1 text-foreground">vigily channels add</code>.
        </p>
      )}
    </div>
  );
}

const GRID = "md:grid md:grid-cols-[minmax(0,1.5fr)_minmax(0,1.5fr)_minmax(0,0.8fr)_minmax(0,1.4fr)_auto] md:items-center md:gap-x-4";

function ChannelList({ channels, admin }: { channels: Channel[]; admin: boolean }) {
  return (
    <div role="table" aria-labelledby="channels-h" className="text-base">
      <div role="row" className={cn(GRID, "hidden border-b py-2 text-[11px] font-bold uppercase tracking-[0.1em] text-faint md:grid")}>
        <span role="columnheader">Channel</span>
        <span role="columnheader">Delivers to</span>
        <span role="columnheader">Routing</span>
        <span role="columnheader">Last delivery</span>
        <span role="columnheader" className="sr-only">
          Actions
        </span>
      </div>
      {channels.map((c) => (
        <ChannelRow key={c.id} c={c} admin={admin} />
      ))}
    </div>
  );
}

/** What the channel sends to: the state of its main secret, or the e-mail recipients. */
function destination(c: Channel) {
  if (c.kind === "email") {
    const to = c.settings.to ?? [];
    return (
      <span className="break-all">
        {to.length ? to.join(", ") : "no recipients"}
        {c.settings.host && <span className="text-faint"> via {c.settings.host}</span>}
      </span>
    );
  }
  const field = SECRET_FIELDS[c.kind][0][0];
  return <SecretStateText state={c.secrets[field]} brief />;
}

function ChannelRow({ c, admin }: { c: Channel; admin: boolean }) {
  const qc = useQueryClient();
  const [result, setResult] = useState<TestResult | null>(null);
  const test = useMutation({
    mutationFn: () => notifyApi.testChannel(c.id),
    onSuccess: (r) => {
      setResult(r);
      void qc.invalidateQueries({ queryKey: ["channels"] });
    },
    onError: (e) => setResult({ ok: false, error: (e as Error).message, duration_ms: 0 }),
  });
  const resultId = `test-${c.id}`;
  return (
    <div role="row" className="border-b py-2.5">
      <div className={cn(GRID, "flex flex-wrap items-baseline gap-x-3 gap-y-1")}>
        <span role="cell" className="flex min-w-0 basis-full items-center gap-2">
          <Tooltip content={KINDS[c.kind].label}>
            <span className="inline-flex">
              <KindIcon kind={c.kind} />
              <span className="sr-only">{KINDS[c.kind].label}:</span>
            </span>
          </Tooltip>
          <Link to={`/settings/notifications/channels/${encodeURIComponent(c.id)}`} className="block min-h-6 min-w-0 truncate font-semibold leading-6 underline-offset-4 hover:underline pointer-coarse:leading-[44px]">
            {c.name}
          </Link>
          {!c.enabled && <span className="shrink-0 text-sm text-warning">off</span>}
        </span>
        <span role="cell" className="min-w-0 basis-full text-sm text-muted-foreground">
          <span className="text-faint md:hidden">{KINDS[c.kind].label}, </span>
          {destination(c)}
        </span>
        <span role="cell" className="text-sm">
          {c.default ? <span className="font-semibold">Default</span> : <span className="text-muted-foreground">By rule</span>}
        </span>
        <span role="cell" className="min-w-0 text-sm">
          {c.last_delivery ? <DeliveryLine d={c.last_delivery} short /> : <span className="text-faint">Nothing sent yet</span>}
        </span>
        <span role="cell" className="flex gap-2 max-md:mt-1 max-md:basis-full">
          {admin && (
            <Button size="sm" variant="outline" loading={test.isPending} onClick={() => test.mutate()} aria-describedby={result ? resultId : undefined}>
              {!test.isPending && <Send />} Test
            </Button>
          )}
          <Button asChild size="sm" variant="ghost">
            <Link to={`/settings/notifications/channels/${encodeURIComponent(c.id)}`} aria-label={`${admin && c.source !== "config" ? "Edit" : "Open"} ${c.name}`}>
              {admin && c.source !== "config" ? "Edit" : "Open"}
            </Link>
          </Button>
        </span>
      </div>
      {c.last_delivery?.status === "failed" && c.last_delivery.error && !result && (
        <p className="mt-1 break-words text-sm text-muted-foreground md:pl-6">{c.last_delivery.error}</p>
      )}
      {result && (
        <div className="mt-1.5 md:pl-6">
          <TestOutcome result={result} id={resultId} />
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// Routing rules

function RulesSection({ channels, admin, rules, error, onRetry }: { channels: Channel[]; admin: boolean; rules: Rules | undefined; error: Error | null; onRetry: () => void }) {
  const qc = useQueryClient();
  const [draft, setDraft] = useState<{ version: number; rules: Rule[] } | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [lost, setLost] = useState<{ message: string; rules: Rule[] } | null>(null);
  const save = useMutation({
    mutationFn: (d: { version: number; rules: Rule[] }) => notifyApi.setRules(d.version, d.rules),
    onSuccess: (r) => {
      qc.setQueryData(["rules"], r);
      setDraft(null);
      setProblem(null);
      setLost(null);
      toast.success(`Routing rules saved (version ${r.version})`);
    },
    onError: async (e, d) => {
      if (e instanceof ApiError && e.status === 409) {
        // Someone saved in between: show their version, keep ours to reapply on top of it.
        await qc.invalidateQueries({ queryKey: ["rules"] });
        setLost({ message: e.message, rules: d.rules });
        setDraft(null);
        setProblem(null);
      } else setProblem((e as Error).message);
    },
  });
  const defaults = channels.filter((c) => c.default && c.enabled);
  const name = (id: string) => channels.find((c) => c.id === id)?.name ?? id;
  const edit = (from: Rule[]) => {
    if (!rules) return;
    setDraft({ version: rules.version, rules: structuredClone(from) });
    setProblem(null);
  };
  const local = draft ? validate(draft.rules) : null;
  return (
    <>
      <SectionHead
        id="rules-h"
        title="Routing rules"
        tools={
          rules && (
            <>
              {rules.version > 0 && rules.updated_at && (
                <Tooltip content={utc(rules.updated_at)}>
                  <span className="nums">
                    Version {rules.version}
                    {rules.changed_by && `, saved by ${rules.changed_by}`} {ago(rules.updated_at)}
                  </span>
                </Tooltip>
              )}
              {admin && !draft && channels.length > 0 && (
                <Button size="sm" variant="outline" onClick={() => edit(rules.rules)}>
                  Edit the rules
                </Button>
              )}
            </>
          )
        }
      />
      {lost && rules && (
        <Insight
          tag="Not saved"
          tone="bad"
          action={
            admin && (
              <button type="button" className="min-h-6 underline underline-offset-4" onClick={() => (edit(lost.rules), setLost(null))}>
                Edit again with my changes
              </button>
            )
          }
        >
          Someone saved the rules while you edited them: {lost.message}. The list below is the current version {rules.version}.
        </Insight>
      )}
      {error && <ErrorState error={error} onRetry={onRetry} className="flex flex-col items-start gap-3 py-6" />}
      {!rules && !error && <Skeleton className="mt-3 h-16" />}
      {rules && !draft && (
        <div>
          {rules.rules.length > 0 && (
            <ol className="text-base">
              {rules.rules.map((r, i) => (
                <li key={i} className="grid grid-cols-[2rem_minmax(0,1fr)] gap-x-2 border-b py-2.5">
                  <span className="font-mono text-sm text-faint nums">{i + 1}</span>
                  <span className="min-w-0">
                    <span className="font-semibold">{r.name || `Rule ${i + 1}`}</span>
                    <span className="block text-sm text-muted-foreground">
                      {matchText(r)}: {r.channels.length ? <>send to {r.channels.map(name).join(", ")}</> : "send nowhere"}
                      {r.stop && <span className="font-semibold text-foreground">; stop here</span>}.
                    </span>
                  </span>
                </li>
              ))}
            </ol>
          )}
          <p className="max-w-4xl py-3 text-sm text-muted-foreground">
            {rules.rules.length ? "Then the default rule: every alert no rule stopped goes" : "No rules yet, so every alert goes"} to the channels marked default
            {defaults.length ? (
              <>
                : <span className="text-foreground">{defaults.map((c) => c.name).join(", ")}</span>.
              </>
            ) : (
              <>. No enabled channel is marked default{rules.rules.length ? "" : ", so alerts are not sent anywhere yet"}.</>
            )}{" "}
            Rules match an alert&apos;s kind, severity, and experiment, suite or schedule name (globs: <code className="font-mono">*</code> any characters,{" "}
            <code className="font-mono">?</code> one).
          </p>
        </div>
      )}
      {draft && (
        <form
          className="max-w-4xl"
          aria-labelledby="rules-h"
          onSubmit={(e) => {
            e.preventDefault();
            if (!local) save.mutate(draft);
          }}
        >
          <ol>
            {draft.rules.map((r, i) => (
              <RuleEditor
                key={i}
                index={i}
                count={draft.rules.length}
                rule={r}
                channels={channels}
                onChange={(next) => setDraft({ ...draft, rules: draft.rules.map((x, j) => (j === i ? next : x)) })}
                onMove={(to) => {
                  const rs = [...draft.rules];
                  const [x] = rs.splice(i, 1);
                  rs.splice(to, 0, x);
                  setDraft({ ...draft, rules: rs });
                }}
                onRemove={() => setDraft({ ...draft, rules: draft.rules.filter((_, j) => j !== i) })}
              />
            ))}
          </ol>
          {draft.rules.length === 0 && <p className="border-b py-4 text-sm text-muted-foreground">No rules: every alert goes to the channels marked default.</p>}
          <div className="flex flex-wrap items-center gap-2 py-3">
            <Button type="button" variant="outline" onClick={() => setDraft({ ...draft, rules: [...draft.rules, { match: {}, channels: [] }] })}>
              <Plus /> Add a rule
            </Button>
            <span className="flex-1" />
            <Button type="button" variant="ghost" onClick={() => (setDraft(null), setProblem(null))}>
              Cancel
            </Button>
            <Button type="submit" variant="brand" loading={save.isPending} disabled={!!local}>
              Save the rules
            </Button>
          </div>
          {(local || problem) && (
            <p role="alert" className="pb-3 text-sm text-danger">
              {local ?? problem}
            </p>
          )}
        </form>
      )}
    </>
  );
}

/** The engine refuses a rule with no channels unless it stops; say so before sending. */
function validate(rules: Rule[]): string | null {
  const i = rules.findIndex((r) => r.channels.length === 0 && !r.stop);
  return i >= 0 ? `Rule ${i + 1} sends nowhere: pick a channel, or tick "Stop here" to silence the alerts it matches.` : null;
}

function matchText(r: Rule): string {
  const m = r.match;
  const parts: string[] = [];
  if (m.kinds?.length) parts.push(m.kinds.map((k) => (ALERT_KINDS.find(([v]) => v === k)?.[1] ?? k).toLowerCase()).join(" or "));
  if (m.severities?.length) parts.push(m.severities.join(" or "));
  if (m.experiment) parts.push(`experiment ${m.experiment}`);
  if (m.suite) parts.push(`suite ${m.suite}`);
  if (m.schedule) parts.push(`schedule ${m.schedule}`);
  return parts.length ? `${parts[0][0].toUpperCase()}${parts.join(", ").slice(1)}` : "Every alert";
}

function toggle<T>(list: T[] | undefined, v: T, on: boolean): T[] | undefined {
  const next = (list ?? []).filter((x) => x !== v);
  if (on) next.push(v);
  return next.length ? next : undefined;
}

const groupLabel = "text-[11px] font-bold uppercase tracking-[0.1em] text-faint";
const check = "inline-flex min-h-6 items-center gap-2 text-sm pointer-coarse:min-h-11";

function RuleEditor({
  index,
  count,
  rule,
  channels,
  onChange,
  onMove,
  onRemove,
}: {
  index: number;
  count: number;
  rule: Rule;
  channels: Channel[];
  onChange: (r: Rule) => void;
  onMove: (to: number) => void;
  onRemove: () => void;
}) {
  const id = `rule-${index}`;
  const label = rule.name || `Rule ${index + 1}`;
  const glob = (key: "experiment" | "suite" | "schedule") => (
    <label className="flex min-w-0 flex-col gap-1">
      <span className="text-sm text-muted-foreground">{key[0].toUpperCase() + key.slice(1)}</span>
      <Input
        className="font-mono"
        placeholder="any"
        spellCheck={false}
        value={rule.match[key] ?? ""}
        onChange={(e) => onChange({ ...rule, match: { ...rule.match, [key]: e.target.value || undefined } })}
      />
    </label>
  );
  return (
    <li className="border-b py-3" aria-labelledby={`${id}-n`}>
      <fieldset className="flex flex-col gap-3">
        <legend className="sr-only">{label}</legend>
        <div className="flex flex-wrap items-center gap-2">
          <span id={`${id}-n`} className="w-6 font-mono text-sm text-faint nums">
            {index + 1}
          </span>
          <Input aria-label={`Name of rule ${index + 1}`} placeholder={`Rule ${index + 1} (optional name)`} value={rule.name ?? ""} onChange={(e) => onChange({ ...rule, name: e.target.value || undefined })} className="w-auto min-w-0 flex-1 basis-48" />
          <span className="flex gap-1">
            <Button size="icon-sm" variant="ghost" aria-label={`Move ${label} up`} disabled={index === 0} onClick={() => onMove(index - 1)}>
              <ArrowUp />
            </Button>
            <Button size="icon-sm" variant="ghost" aria-label={`Move ${label} down`} disabled={index === count - 1} onClick={() => onMove(index + 1)}>
              <ArrowDown />
            </Button>
            <Button size="icon-sm" variant="ghost" aria-label={`Remove ${label}`} onClick={onRemove}>
              <Trash2 />
            </Button>
          </span>
        </div>
        <div className="grid gap-x-6 gap-y-3 sm:grid-cols-2 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)] sm:pl-8">
          <div role="group" aria-label="Alert kinds" className="flex flex-col gap-1">
            <span className={groupLabel}>Kinds</span>
            <span className="flex flex-wrap gap-x-4">
              {ALERT_KINDS.map(([k, l]) => (
                <label key={k} className={check}>
                  <input type="checkbox" checked={!!rule.match.kinds?.includes(k)} onChange={(e) => onChange({ ...rule, match: { ...rule.match, kinds: toggle(rule.match.kinds, k, e.target.checked) } })} />
                  {l}
                </label>
              ))}
            </span>
          </div>
          <div role="group" aria-label="Severities" className="flex flex-col gap-1">
            <span className={groupLabel}>Severities</span>
            <span className="flex flex-wrap gap-x-4">
              {SEVERITIES.map(([k, l]) => (
                <label key={k} className={check}>
                  <input
                    type="checkbox"
                    checked={!!rule.match.severities?.includes(k)}
                    onChange={(e) => onChange({ ...rule, match: { ...rule.match, severities: toggle(rule.match.severities, k, e.target.checked) } })}
                  />
                  {l}
                </label>
              ))}
            </span>
          </div>
          <p className="-mt-1 text-sm text-faint sm:col-span-2">A rule with nothing ticked or filled in for a field matches every alert on that field.</p>
          <div className="grid gap-3 sm:col-span-2 sm:grid-cols-3">
            {glob("experiment")}
            {glob("suite")}
            {glob("schedule")}
          </div>
          <div role="group" aria-label="Send to" className="flex flex-col gap-1 sm:col-span-2">
            <span className={groupLabel}>Send to</span>
            <span className="flex flex-wrap gap-x-4">
              {channels.map((c) => (
                <label key={c.id} className={check}>
                  <input type="checkbox" checked={rule.channels.includes(c.id)} onChange={(e) => onChange({ ...rule, channels: toggle(rule.channels, c.id, e.target.checked) ?? [] })} />
                  <KindIcon kind={c.kind} />
                  {c.name}
                  {!c.enabled && <span className="text-warning">(off)</span>}
                </label>
              ))}
            </span>
            <label className={cn(check, "mt-1")}>
              <input type="checkbox" checked={!!rule.stop} onChange={(e) => onChange({ ...rule, stop: e.target.checked || undefined })} />
              Stop here: later rules and the default rule skip the alerts this rule matches
            </label>
          </div>
        </div>
      </fieldset>
    </li>
  );
}
