import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Send, Trash2 } from "lucide-react";
import { type FormEvent, useId, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";
import { toast } from "sonner";
import { DeliveryLine, KIND_ORDER, KindIcon, KINDS, SecretStateText, SettingsNav, sentence, TestOutcome, useCanAdmin } from "@/components/notifications";
import { Insight, PageHeader, SectionHead } from "@/components/page";
import { ErrorState } from "@/components/states";
import { Button } from "@/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import {
  type Channel,
  type ChannelKind,
  type ChannelSettings,
  notifyApi,
  SECRET_FIELDS,
  type SecretInput,
  type SecretState,
  type SecretsStatus,
  type SmtpSecurity,
  type TestResult,
} from "@/lib/api";
import { useCrumbs } from "@/lib/crumbs";
import { utc } from "@/lib/format";
import { useTitle } from "@/lib/hooks";
import { cn } from "@/lib/utils";
import { useSecrets } from "@/pages/notifications";

const FIELD_LABEL: Record<string, string> = { webhook_url: "Webhook URL", url: "URL", signing_key: "Signing key", password: "SMTP password" };

const HELP: Record<ChannelKind, React.ReactNode> = {
  slack: (
    <>
      In Slack, create an app with <i>Incoming Webhooks</i> turned on, add a webhook to the channel that should receive alerts, and paste its URL (
      <code className="font-mono">https://hooks.slack.com/services/…</code>). Messages use Block Kit.
    </>
  ),
  teams: (
    <>
      In Microsoft Teams, add the Workflows template <i>Post to a channel when a webhook request is received</i> (or an incoming webhook connector) to the channel and paste the URL it gives. Messages
      are Adaptive Cards.
    </>
  ),
  discord: <>In Discord, open the channel&apos;s settings, then Integrations, Webhooks, New Webhook, and copy its URL. Messages are embeds.</>,
  webhook: (
    <>
      vigily posts its JSON alert document (<code className="font-mono">vigily.alert/1</code>) to this URL. With a signing key, each request carries{" "}
      <code className="font-mono">X-Vigily-Timestamp</code> and <code className="font-mono">X-Vigily-Signature: sha256=…</code>, an HMAC-SHA256 of the timestamp, a dot and the body, so the receiver can
      check that vigily sent it.
    </>
  ),
  email: <>vigily sends a plain-text and HTML message per alert through this SMTP server. STARTTLS on port 587 suits most providers; TLS uses port 465.</>,
};

const DEFAULT_PORT: Record<SmtpSecurity, number> = { starttls: 587, tls: 465, none: 25 };

type Mode = "keep" | "value" | "env" | "remove";
interface SecretDraft {
  mode: Mode;
  value: string;
  env: string;
}

function initialSecret(state: SecretState | undefined, canStore: boolean): SecretDraft {
  const set = !!state && state.source !== "none";
  const env = state?.source.startsWith("env:") ? state.source.slice(4) : "";
  return { mode: set ? "keep" : canStore ? "value" : "env", value: "", env };
}

function secretInput(d: SecretDraft): SecretInput | undefined {
  // An empty value field leaves the secret as it is; "Remove" deletes it.
  if (d.mode === "value") return d.value ? { value: d.value } : undefined;
  if (d.mode === "env") return { env: d.env.trim() };
  if (d.mode === "remove") return { value: "" };
  return undefined;
}

const ENV_NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;

interface Draft {
  kind: ChannelKind;
  name: string;
  enabled: boolean;
  default: boolean;
  host: string;
  port: string;
  security: SmtpSecurity;
  username: string;
  from: string;
  to: string;
  burst: string;
  per_minute: string;
  secrets: Record<string, SecretDraft>;
}

function draftOf(kind: ChannelKind, c: Channel | undefined, canStore: boolean): Draft {
  const s = c?.settings ?? {};
  return {
    kind,
    name: c?.name ?? "",
    enabled: c?.enabled ?? true,
    default: c?.default ?? false,
    host: s.host ?? "",
    port: s.port ? String(s.port) : "",
    security: s.security ?? "starttls",
    username: s.username ?? "",
    from: s.from ?? "",
    to: (s.to ?? []).join("\n"),
    burst: s.burst !== undefined ? String(s.burst) : "",
    per_minute: s.per_minute !== undefined ? String(s.per_minute) : "",
    secrets: Object.fromEntries(SECRET_FIELDS[kind].map(([f]) => [f, initialSecret(c?.secrets[f], canStore)])),
  };
}

function settingsOf(d: Draft): ChannelSettings {
  const n = (v: string) => (v.trim() === "" ? undefined : Number(v));
  const t = (v: string) => v.trim() || undefined;
  const common = { burst: n(d.burst), per_minute: n(d.per_minute) };
  if (d.kind !== "email") return common;
  return {
    ...common,
    host: t(d.host),
    port: n(d.port),
    security: d.security,
    username: t(d.username),
    from: t(d.from),
    to: d.to
      .split(/[\n,]/)
      .map((x) => x.trim())
      .filter(Boolean),
  };
}

/** What is wrong with the form before it is sent; null when it can be saved. */
function problemOf(d: Draft, isNew: boolean): string | null {
  if (!d.name.trim()) return "Give the channel a name.";
  for (const [f, required] of SECRET_FIELDS[d.kind]) {
    const s = d.secrets[f];
    if (s.mode === "env" && !ENV_NAME.test(s.env.trim())) return `${FIELD_LABEL[f]}: an environment variable name has letters, digits and underscores, and does not start with a digit.`;
    if (s.mode === "value" && !s.value && required && isNew) return `${FIELD_LABEL[f]}: enter a value, or name an environment variable.`;
  }
  if (d.kind === "email") {
    if (!d.host.trim()) return "Enter the SMTP server's host name.";
    if (!d.from.trim()) return "Enter the sender address (From).";
    if (!settingsOf(d).to?.length) return "Enter at least one recipient.";
    if (d.port && !(Number(d.port) > 0 && Number(d.port) < 65536)) return "The port is a number from 1 to 65535.";
  }
  for (const [k, l] of [
    ["burst", "Burst"],
    ["per_minute", "Per minute"],
  ] as const)
    if (d[k] && !(Number.isInteger(Number(d[k])) && Number(d[k]) > 0)) return `${l} is a whole number above zero.`;
  return null;
}

/** Add a channel (`/settings/notifications/channels/new`) or show and change one. */
export function ChannelPage() {
  const { id } = useParams();
  const isNew = id === undefined;
  const channel = useQuery({ queryKey: ["channel", id], queryFn: ({ signal }) => notifyApi.channel(id!, signal), enabled: !isNew });
  const secrets = useSecrets();
  const title = isNew ? "Add a channel" : (channel.data?.name ?? "Channel");
  useTitle(title);
  useCrumbs([{ label: "Settings", to: "/settings" }, { label: "Notifications", to: "/settings/notifications" }, { label: title }]);
  if (channel.isError) return <ErrorState error={channel.error as Error} onRetry={() => channel.refetch()} />;
  if (secrets.isError) return <ErrorState error={secrets.error as Error} onRetry={() => secrets.refetch()} />;
  if ((!isNew && !channel.data) || !secrets.data)
    return (
      <div className="px-4 pt-5 md:px-6">
        <Skeleton className="h-10 w-72" />
        <Skeleton className="mt-6 h-64 max-w-3xl" />
      </div>
    );
  return <ChannelForm key={channel.data?.updated_at ?? "new"} channel={channel.data} secrets={secrets.data} />;
}

function ChannelForm({ channel, secrets }: { channel: Channel | undefined; secrets: SecretsStatus }) {
  const isNew = !channel;
  const nav = useNavigate();
  const qc = useQueryClient();
  const [sp, setSp] = useSearchParams();
  const { admin } = useCanAdmin();
  const fromConfig = channel?.source === "config";
  const readOnly = !admin || fromConfig;
  const startKind = (KIND_ORDER as string[]).includes(sp.get("kind") ?? "") ? (sp.get("kind") as ChannelKind) : "slack";
  const [d, setD] = useState<Draft>(() => draftOf(channel?.kind ?? startKind, channel, secrets.can_store));
  const [tried, setTried] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<TestResult | null>(null);
  const [confirm, setConfirm] = useState(false);
  const set = <K extends keyof Draft>(k: K, v: Draft[K]) => setD((p) => ({ ...p, [k]: v }));
  const problem = problemOf(d, isNew);

  const save = useMutation({
    mutationFn: () => {
      const sec = Object.fromEntries(
        Object.entries(d.secrets)
          .map(([f, s]) => [f, secretInput(s)] as const)
          .filter(([, v]) => v !== undefined),
      ) as Record<string, SecretInput>;
      const body = { name: d.name.trim(), enabled: d.enabled, default: d.default, settings: settingsOf(d), secrets: sec };
      return channel ? notifyApi.updateChannel(channel.id, body) : notifyApi.createChannel({ ...body, kind: d.kind });
    },
    onSuccess: (c) => {
      void qc.invalidateQueries({ queryKey: ["channels"] });
      void qc.invalidateQueries({ queryKey: ["secrets"] });
      qc.setQueryData(["channel", c.id], c);
      toast.success(isNew ? `Channel ${c.name} added` : `Channel ${c.name} saved`);
      nav(isNew ? `/settings/notifications/channels/${encodeURIComponent(c.id)}` : "/settings/notifications", { replace: isNew });
    },
    onError: (e) => setError((e as Error).message),
  });
  const test = useMutation({
    mutationFn: () => notifyApi.testChannel(channel!.id),
    onSuccess: (r) => {
      setResult(r);
      void qc.invalidateQueries({ queryKey: ["channel", channel!.id] });
      void qc.invalidateQueries({ queryKey: ["channels"] });
    },
    onError: (e) => setResult({ ok: false, error: (e as Error).message, duration_ms: 0 }),
  });
  const remove = useMutation({
    mutationFn: () => notifyApi.deleteChannel(channel!.id),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["channels"] });
      void qc.invalidateQueries({ queryKey: ["rules"] });
      toast.success(`Channel ${channel!.name} deleted`);
      nav("/settings/notifications");
    },
    onError: (e) => {
      setConfirm(false);
      setError((e as Error).message);
    },
  });

  const submit = (e: FormEvent) => {
    e.preventDefault();
    setTried(true);
    setError(null);
    if (!problem) save.mutate();
  };
  const row = "grid gap-1.5 border-b py-3 sm:grid-cols-[200px_minmax(0,1fr)] sm:gap-6";
  const label = "pt-1.5 text-sm text-faint";
  const dirty = isNew || JSON.stringify(d) !== JSON.stringify(draftOf(d.kind, channel, secrets.can_store));

  return (
    <div className="px-4 pb-16 pt-5 md:px-6">
      <SettingsNav />
      <PageHeader
        title={
          isNew ? (
            "Add a channel"
          ) : (
            <span className="inline-flex items-center gap-2">
              <KindIcon kind={channel.kind} className="size-5" />
              {channel.name}
            </span>
          )
        }
        actions={
          channel &&
          admin && (
            <>
              <Button variant="outline" loading={test.isPending} onClick={() => test.mutate()}>
                {!test.isPending && <Send />} Send a test alert
              </Button>
              {!fromConfig && (
                <Button variant="outline" onClick={() => setConfirm(true)}>
                  <Trash2 /> Delete
                </Button>
              )}
            </>
          )
        }
      >
        {channel && (
          <>
            <span>{KINDS[channel.kind].label}</span>
            <span>{fromConfig ? "From vigily.toml" : channel.changed_by ? `Changed by ${channel.changed_by}` : "Added with the API"}</span>
            <span className="nums">Updated {utc(channel.updated_at)}</span>
            <span>Last delivery: {channel.last_delivery ? <DeliveryLine d={channel.last_delivery} short /> : "none yet"}</span>
          </>
        )}
      </PageHeader>
      {result && (
        <div className="border-b py-3">
          <TestOutcome result={result} />
        </div>
      )}
      {channel?.last_delivery?.status === "failed" && channel.last_delivery.error && !result && (
        <Insight tag="Last delivery failed" tone="bad">
          {channel.last_delivery.error}
          {channel.last_delivery.status_code !== undefined && ` (${channel.last_delivery.status_code})`}
        </Insight>
      )}
      {fromConfig && (
        <Insight tag="Read only" tone="plain">
          This channel is the deprecated <code className="font-mono">[alerts] webhook</code> in vigily.toml. Change or remove it there, or replace it with a webhook channel here. It can still be tested.
        </Insight>
      )}
      {!admin && !fromConfig && (
        <Insight tag="Read only" tone="plain">
          Changing channels needs a token with the admin scope.
        </Insight>
      )}

      <form onSubmit={submit} className="max-w-3xl" noValidate>
        {isNew && (
          <>
            <SectionHead id="kind-h" title="Kind" />
            <div role="radiogroup" aria-labelledby="kind-h" className="grid grid-cols-2 gap-px border-b py-3 sm:grid-cols-5">
              {KIND_ORDER.map((k) => (
                <button
                  key={k}
                  type="button"
                  role="radio"
                  aria-checked={d.kind === k}
                  onClick={() => {
                    setD((p) => ({ ...p, kind: k, secrets: draftOf(k, undefined, secrets.can_store).secrets }));
                    setSp((p) => (p.set("kind", k), p), { replace: true });
                  }}
                  className={cn(
                    "flex min-h-11 items-center gap-2 border px-3 text-left text-sm transition-[background-color,color] duration-[120ms]",
                    d.kind === k ? "border-foreground bg-card font-semibold text-foreground" : "border-border text-muted-foreground hover:bg-hover hover:text-foreground",
                  )}
                >
                  <KindIcon kind={k} className={d.kind === k ? "text-foreground" : undefined} />
                  {KINDS[k].label}
                </button>
              ))}
            </div>
          </>
        )}

        <SectionHead title="Channel" />
        <fieldset disabled={readOnly} className="contents">
          <div className={row}>
            <label htmlFor="ch-name" className={label}>
              Name
            </label>
            <Input id="ch-name" value={d.name} onChange={(e) => set("name", e.target.value)} className="h-9 max-w-md" autoComplete="off" />
          </div>
          <div className={row}>
            <span className={label}>Sending</span>
            <div className="flex flex-col gap-1">
              <label className="inline-flex min-h-6 items-center gap-2 text-base pointer-coarse:min-h-11">
                <input type="checkbox" checked={d.enabled} onChange={(e) => set("enabled", e.target.checked)} />
                Enabled
              </label>
              <label className="inline-flex min-h-6 items-center gap-2 text-base pointer-coarse:min-h-11">
                <input type="checkbox" checked={d.default} onChange={(e) => set("default", e.target.checked)} />
                Default: every alert no routing rule stopped comes here
              </label>
            </div>
          </div>
        </fieldset>

        <SectionHead title={d.kind === "email" ? "SMTP server" : "Destination"} />
        <div className="border-b py-3">
          <p className="max-w-prose text-sm text-muted-foreground">{HELP[d.kind]}</p>
        </div>
        <fieldset disabled={readOnly} className="contents">
          {d.kind === "email" && (
            <>
              <div className={row}>
                <label htmlFor="ch-host" className={label}>
                  Host
                </label>
                <Input id="ch-host" value={d.host} onChange={(e) => set("host", e.target.value)} placeholder="smtp.example.com" className="h-9 max-w-md font-mono" spellCheck={false} />
              </div>
              <div className={row}>
                <label htmlFor="ch-security" className={label}>
                  Security
                </label>
                <div className="flex flex-wrap items-center gap-3">
                  <select id="ch-security" value={d.security} onChange={(e) => set("security", e.target.value as SmtpSecurity)} className="h-9 border px-2.5 text-sm disabled:cursor-not-allowed disabled:border-border disabled:bg-transparent">
                    <option value="starttls">STARTTLS</option>
                    <option value="tls">TLS</option>
                    <option value="none">None (trusted relay only)</option>
                  </select>
                  <label htmlFor="ch-port" className="text-sm text-faint">
                    Port
                  </label>
                  <Input id="ch-port" aria-describedby="ch-port-help" inputMode="numeric" value={d.port} onChange={(e) => set("port", e.target.value)} className="h-9 w-24 font-mono nums" />
                  <span id="ch-port-help" className="basis-full text-sm text-faint">
                    Left empty: {DEFAULT_PORT[d.security]} for {d.security === "none" ? "no encryption" : d.security === "tls" ? "TLS" : "STARTTLS"}.
                  </span>
                </div>
              </div>
              <div className={row}>
                <label htmlFor="ch-user" className={label}>
                  User name
                </label>
                <Input id="ch-user" value={d.username} onChange={(e) => set("username", e.target.value)} className="h-9 max-w-md font-mono" autoComplete="off" spellCheck={false} />
              </div>
            </>
          )}
          {SECRET_FIELDS[d.kind].map(([f, required]) => (
            <SecretField
              key={`${d.kind}-${f}`}
              field={f}
              required={required}
              state={channel?.secrets[f]}
              draft={d.secrets[f]}
              onChange={(s) => setD((p) => ({ ...p, secrets: { ...p.secrets, [f]: s } }))}
              status={secrets}
              isNew={isNew}
              readOnly={readOnly}
              rowClass={row}
              labelClass={label}
            />
          ))}
          {d.kind === "email" && (
            <>
              <div className={row}>
                <label htmlFor="ch-from" className={label}>
                  From
                </label>
                <Input id="ch-from" value={d.from} onChange={(e) => set("from", e.target.value)} placeholder="vigily <vigily@example.com>" className="h-9 max-w-md" spellCheck={false} />
              </div>
              <div className={row}>
                <label htmlFor="ch-to" className={label}>
                  To
                </label>
                <div className="flex max-w-md flex-col gap-1">
                  <textarea
                    id="ch-to"
                    value={d.to}
                    onChange={(e) => set("to", e.target.value)}
                    rows={3}
                    placeholder="oncall@example.com"
                    spellCheck={false}
                    aria-describedby="ch-to-help"
                    className="w-full border border-input bg-card px-2.5 py-1.5 font-mono text-sm disabled:cursor-not-allowed disabled:border-border disabled:bg-transparent"
                  />
                  <span id="ch-to-help" className="text-sm text-faint">
                    One address per line, or separated by commas.
                  </span>
                </div>
              </div>
            </>
          )}
        </fieldset>

        <SectionHead title="Rate limit" />
        <fieldset disabled={readOnly} className="contents">
          <div className={row}>
            <span className={label}>Messages</span>
            <div className="flex flex-col gap-1.5">
              <div className="flex flex-wrap items-center gap-x-3 gap-y-2 text-sm">
                <label htmlFor="ch-burst" className="text-faint">
                  Burst
                </label>
                <Input id="ch-burst" inputMode="numeric" value={d.burst} onChange={(e) => set("burst", e.target.value)} className="h-9 w-20 font-mono nums" />
                <label htmlFor="ch-rate" className="text-faint">
                  then per minute
                </label>
                <Input id="ch-rate" inputMode="numeric" value={d.per_minute} onChange={(e) => set("per_minute", e.target.value)} className="h-9 w-20 font-mono nums" />
              </div>
              <span className="text-sm text-faint">Left empty: a burst of 5, then 20 per minute. Alerts over the limit wait for their turn; none are dropped. A test send counts too.</span>
            </div>
          </div>
        </fieldset>

        {!readOnly && (
          <div className="flex flex-col gap-2 py-4">
            {((tried && problem) || error) && (
              <p role="alert" className="text-sm text-danger">
                {(tried && problem) || error}
              </p>
            )}
            <div className="flex flex-wrap gap-2">
              <Button type="submit" variant="brand" loading={save.isPending} disabled={!dirty}>
                {isNew ? "Add the channel" : "Save changes"}
              </Button>
              <Button asChild variant="ghost">
                <Link to="/settings/notifications">Cancel</Link>
              </Button>
            </div>
          </div>
        )}
      </form>
      {channel && (
        <Dialog open={confirm} onOpenChange={setConfirm}>
          <DialogContent className="max-w-md border border-foreground p-5">
            <DialogTitle className="text-lg font-semibold">Delete {channel.name}?</DialogTitle>
            <DialogDescription className="mt-2 text-sm text-muted-foreground">
              Its stored secrets are deleted with it, and routing rules stop sending to it. Alerts already delivered keep their delivery log. This cannot be undone.
            </DialogDescription>
            <div className="mt-5 flex justify-end gap-2">
              <DialogClose asChild>
                <Button variant="secondary">Cancel</Button>
              </DialogClose>
              <Button variant="danger" loading={remove.isPending} onClick={() => remove.mutate()}>
                <Trash2 /> Delete the channel
              </Button>
            </div>
          </DialogContent>
        </Dialog>
      )}
    </div>
  );
}

/** A write-only secret: its current state, and keep it, store a new value, read an environment variable, or remove it. */
function SecretField({
  field,
  required,
  state,
  draft,
  onChange,
  status,
  isNew,
  readOnly,
  rowClass,
  labelClass,
}: {
  field: string;
  required: boolean;
  state: SecretState | undefined;
  draft: SecretDraft;
  onChange: (d: SecretDraft) => void;
  status: SecretsStatus;
  isNew: boolean;
  readOnly: boolean;
  rowClass: string;
  labelClass: string;
}) {
  const id = useId();
  const set = !!state && state.source !== "none";
  const modes: [Mode, string][] = [
    ...(isNew || !set ? [] : ([["keep", "Keep"]] as [Mode, string][])),
    ["value", "New value"],
    ["env", "Environment variable"],
    ...(!required && set && !isNew ? ([["remove", "Remove"]] as [Mode, string][]) : []),
  ];
  const noKey = !status.can_store;
  const description = `${id}-state`;
  return (
    <div className={rowClass}>
      <span className={labelClass} id={`${id}-l`}>
        {FIELD_LABEL[field] ?? field}
        {!required && <span className="block text-xs">optional</span>}
      </span>
      <div className="flex min-w-0 flex-col gap-2">
        {!isNew && (
          <p id={description} className="text-sm">
            <span className="text-faint">Now: </span>
            <SecretStateText state={state} />
          </p>
        )}
        {!readOnly && (
          <>
            <span role="radiogroup" aria-labelledby={`${id}-l`} className="flex flex-wrap gap-x-[18px]">
              {modes.map(([m, l]) => {
                const disabled = m === "value" && noKey;
                return (
                  <button
                    key={m}
                    type="button"
                    role="radio"
                    aria-checked={draft.mode === m}
                    disabled={disabled}
                    onClick={() => onChange({ ...draft, mode: m })}
                    className={cn(
                      "min-h-6 text-sm underline-offset-[5px] disabled:cursor-not-allowed disabled:opacity-45 pointer-coarse:min-h-11",
                      draft.mode === m ? "font-bold text-foreground underline decoration-2" : "text-muted-foreground hover:text-foreground",
                    )}
                  >
                    {l}
                  </button>
                );
              })}
            </span>
            {draft.mode === "value" && (
              <Input
                aria-labelledby={`${id}-l`}
                aria-describedby={isNew ? undefined : description}
                type="password"
                autoComplete="off"
                spellCheck={false}
                disabled={noKey}
                value={draft.value}
                onChange={(e) => onChange({ ...draft, value: e.target.value })}
                placeholder={field === "signing_key" ? "a long random string, shared with the receiver" : field === "password" ? "" : "https://…"}
                className="h-9 font-mono"
              />
            )}
            {draft.mode === "env" && (
              <div className="flex flex-col gap-1">
                <Input
                  aria-label={`${FIELD_LABEL[field]}: environment variable name`}
                  value={draft.env}
                  onChange={(e) => onChange({ ...draft, env: e.target.value })}
                  placeholder={field === "password" ? "SMTP_PASSWORD" : field === "signing_key" ? "WEBHOOK_SIGNING_KEY" : "SLACK_WEBHOOK_URL"}
                  className="h-9 max-w-xs font-mono"
                  spellCheck={false}
                  autoComplete="off"
                />
                <span className="text-sm text-faint">The engine reads this variable from its own environment each time it sends; the value never leaves the engine.</span>
              </div>
            )}
            {draft.mode === "remove" && <span className="text-sm text-muted-foreground">The stored value is deleted when you save.</span>}
            {noKey && status.message && <span className="text-sm text-muted-foreground">{sentence(status.message)}</span>}
            {draft.mode === "value" && !noKey && <span className="text-sm text-faint">Stored encrypted; it cannot be shown again, only replaced.</span>}
          </>
        )}
      </div>
    </div>
  );
}
