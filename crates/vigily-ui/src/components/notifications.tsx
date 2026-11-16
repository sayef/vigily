import { useQuery } from "@tanstack/react-query";
import { Hash, Mail, MessageSquare, TriangleAlert, Users, Webhook } from "lucide-react";
import { NavLink } from "react-router";
import { Tooltip } from "@/components/ui/tooltip";
import { api, type ChannelKind, type Delivery, type DeliverySummary, type SecretState, type TestResult } from "@/lib/api";
import { ago, utc } from "@/lib/format";
import { cn } from "@/lib/utils";

export const KINDS: Record<ChannelKind, { label: string; icon: typeof Hash }> = {
  slack: { label: "Slack", icon: Hash },
  teams: { label: "Microsoft Teams", icon: Users },
  discord: { label: "Discord", icon: MessageSquare },
  webhook: { label: "Webhook", icon: Webhook },
  email: { label: "E-mail", icon: Mail },
};

export const KIND_ORDER: ChannelKind[] = ["slack", "teams", "discord", "webhook", "email"];

export function KindIcon({ kind, className }: { kind: ChannelKind; className?: string }) {
  const Icon = KINDS[kind].icon;
  return <Icon className={cn("size-4 shrink-0 text-muted-foreground", className)} strokeWidth={1.75} aria-hidden />;
}

/** Whether the signed-in principal may change channels, rules and secrets (the admin scope). */
export function useCanAdmin(): { admin: boolean; known: boolean } {
  const me = useQuery({ queryKey: ["me"], queryFn: ({ signal }) => api.me(signal), staleTime: 60_000 });
  return { admin: !!me.data?.scopes.includes("admin"), known: !!me.data };
}

/** The settings pages: General and Notifications. */
export function SettingsNav() {
  const link = ({ isActive }: { isActive: boolean }) =>
    cn(
      "inline-flex min-h-6 items-center text-sm underline-offset-[5px] pointer-coarse:min-h-11",
      isActive ? "font-bold text-foreground underline decoration-2" : "text-muted-foreground hover:text-foreground",
    );
  return (
    <nav aria-label="Settings" className="mb-3 flex gap-x-[18px]">
      <NavLink to="/settings" end className={link}>
        General
      </NavLink>
      <NavLink to="/settings/notifications" className={link}>
        Notifications
      </NavLink>
    </nav>
  );
}

/** "stored · …abcd", "from env NAME", "from vigily.toml" or "not set", with who changed it. */
export function SecretStateText({ state, className, brief }: { state: SecretState | undefined; className?: string; brief?: boolean }) {
  const s = state ?? { source: "none" };
  if (s.undecryptable) {
    return (
      <span className={cn("inline-flex items-baseline gap-1.5 text-danger", className)} role="status">
        <TriangleAlert className="size-3.5 shrink-0 translate-y-0.5" aria-hidden />
        <span>{brief ? "cannot be decrypted" : "cannot be decrypted; store it again or set VIGILY_SECRET_KEY_PREVIOUS"}</span>
      </span>
    );
  }
  let text: React.ReactNode;
  if (s.source === "stored") text = <>stored{s.hint && <span className="font-mono"> · {s.hint}</span>}</>;
  else if (s.source.startsWith("env:"))
    text = (
      <>
        from env <span className="font-mono">{s.source.slice(4)}</span>
      </>
    );
  else if (s.source === "config") text = "from vigily.toml ([alerts] webhook)";
  else text = "not set";
  return (
    <span className={className}>
      {text}
      {!brief && s.changed_by && s.changed_at && (
        <Tooltip content={utc(s.changed_at)}>
          <span className="text-faint">
            , changed by {s.changed_by} {ago(s.changed_at)}
          </span>
        </Tooltip>
      )}
    </span>
  );
}

const STATUS_WORD: Record<Delivery["status"], string> = { sent: "Sent", failed: "Failed", pending: "Retrying" };

/** A delivery's status as a word (never colour alone), its code and when. */
export function DeliveryLine({ d, short }: { d: Delivery; short?: boolean }) {
  const pending = d.status === "pending";
  return (
    <span className="inline-flex min-w-0 flex-wrap items-baseline gap-x-1.5">
      <span className={cn("font-semibold", d.status === "failed" && "text-danger", d.status === "sent" && "text-brand", pending && "text-warning")}>
        {pending && d.attempts === 0 ? "Queued" : STATUS_WORD[d.status]}
      </span>
      {d.status_code !== undefined && <span className="font-mono text-[12.5px] nums">{d.status_code}</span>}
      <Tooltip content={utc(d.updated_at)}>
        <span className="text-faint">
          {d.alert_id === "test" ? "test " : ""}
          {ago(d.updated_at)}
        </span>
      </Tooltip>
      {!short && d.error && <span className="basis-full break-words text-sm text-muted-foreground">{d.error}</span>}
    </span>
  );
}

/** The outcome of "Send a test alert": the error verbatim and the status code. */
export function TestOutcome({ result, id }: { result: TestResult; id?: string }) {
  return (
    <p id={id} role="status" className={cn("flex flex-wrap items-baseline gap-x-2 text-sm", result.ok ? "text-foreground" : "text-danger")}>
      <span className={cn("font-semibold", result.ok ? "text-brand" : "text-danger")}>{result.ok ? "Test alert sent" : "Test failed"}</span>
      {result.status_code !== undefined && <span className="font-mono text-[12.5px] nums">{result.status_code}</span>}
      <span className="text-faint nums">in {result.duration_ms} ms</span>
      {result.error && <span className="basis-full break-words">{result.error}</span>}
    </p>
  );
}

/** A server message as a sentence: its first letter in upper case. */
export const sentence = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

/** "2 sent, 1 failed" for an alert's deliveries. */
export function summaryText(s: DeliverySummary): string {
  const parts = [s.sent && `${s.sent} sent`, s.failed && `${s.failed} failed`, s.pending && `${s.pending} retrying`].filter(Boolean);
  return parts.length ? parts.join(", ") : "no deliveries";
}
