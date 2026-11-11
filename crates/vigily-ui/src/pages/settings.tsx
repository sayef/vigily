import { useState } from "react";
import { DOCS_URL } from "@/components/layout";
import { PageHeader, SectionHead, ToolSwitch } from "@/components/page";
import { ShortcutsDialog } from "@/components/shortcuts";
import { useTheme } from "@/components/theme";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { integer, utc } from "@/lib/format";
import { useTitle } from "@/lib/hooks";
import { useMeta } from "@/lib/queries";

/** What this server reads and how the app looks. Server settings live in its configuration file. */
export function SettingsPage() {
  useTitle("Settings");
  const meta = useMeta();
  const { theme, toggle } = useTheme();
  const [keys, setKeys] = useState(false);
  const m = meta.data;
  const row = "grid gap-1 border-b py-3 sm:grid-cols-[200px_minmax(0,1fr)] sm:gap-6";
  return (
    <div className="px-4 pb-16 pt-5 md:px-6">
      <PageHeader title="Settings">
        <span>Server settings (schedules, drift thresholds, alert delivery) are read from its configuration file at start.</span>
      </PageHeader>

      <SectionHead id="ws-h" title="Workspace" />
      <dl className="text-base">
        <div className={row}>
          <dt className="text-sm text-faint">Repository</dt>
          <dd className="break-all font-mono text-[13px]">{m ? m.repository : <Skeleton className="h-4 w-64" />}</dd>
        </div>
        <div className={row}>
          <dt className="text-sm text-faint">Contents</dt>
          <dd className="nums">
            {m ? (
              <>
                {integer(m.runs)} runs ({integer(m.archived)} archived), {m.experiments.filter((e) => e.name).length} experiments, {m.datasets} datasets, {m.schedules} schedules
              </>
            ) : (
              <Skeleton className="h-4 w-48" />
            )}
          </dd>
        </div>
        <div className={row}>
          <dt className="text-sm text-faint">Read at</dt>
          <dd className="font-mono text-[13px] nums">{m ? utc(m.refreshed_at) : <Skeleton className="h-4 w-40" />}</dd>
        </div>
        <div className={row}>
          <dt className="text-sm text-faint">Server version</dt>
          <dd className="font-mono text-[13px]">{m ? m.version : <Skeleton className="h-4 w-12" />}</dd>
        </div>
      </dl>

      <SectionHead id="app-h" title="This browser" />
      <dl className="text-base">
        <div className={row}>
          <dt className="text-sm text-faint">Theme</dt>
          <dd>
            <ToolSwitch
              label="Theme"
              options={[
                ["light", "Light"],
                ["dark", "Dark"],
              ]}
              value={theme}
              onChange={(t) => t !== theme && toggle()}
            />
          </dd>
        </div>
        <div className={row}>
          <dt className="text-sm text-faint">Keyboard</dt>
          <dd className="flex flex-wrap items-center gap-3">
            <Button variant="outline" onClick={() => setKeys(true)}>
              Show the shortcuts
            </Button>
            <span className="text-sm text-muted-foreground">
              Or press <kbd>?</kbd> anywhere.
            </span>
          </dd>
        </div>
        <div className={row}>
          <dt className="text-sm text-faint">Documentation</dt>
          <dd>
            <a href={DOCS_URL} target="_blank" rel="noreferrer" className="inline-flex min-h-6 items-center font-semibold underline underline-offset-4">
              Guides and reference
            </a>
          </dd>
        </div>
      </dl>
      <ShortcutsDialog open={keys} onOpenChange={setKeys} />
    </div>
  );
}
