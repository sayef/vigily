import { useQuery } from "@tanstack/react-query";
import { Keyboard, Menu, Moon, Search, Sun } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Link, NavLink, Outlet, useLocation, useNavigate } from "react-router";
import { CommandPalette } from "@/components/command-palette";
import { Mark, WordmarkText } from "@/components/logo";
import { ShortcutsDialog } from "@/components/shortcuts";
import { useTheme } from "@/components/theme";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { Kbd } from "@/components/ui/kbd";
import { Tooltip } from "@/components/ui/tooltip";
import { evalApi } from "@/lib/api";
import { CrumbsProvider, useCrumbTrail } from "@/lib/crumbs";
import { integer } from "@/lib/format";
import { useMeta } from "@/lib/queries";
import { cn, modKey, typing } from "@/lib/utils";

export const DOCS_URL = "https://github.com/sayef/vigily/tree/main/docs";

export const NAV = [
  { to: "/", label: "Runs", key: "r", group: "Track" },
  { to: "/metrics", label: "Metrics", key: "m", group: "Track" },
  { to: "/experiments", label: "Experiments", key: "e", group: "Track" },
  { to: "/traces", label: "Traces", key: "t", group: "Track" },
  { to: "/datasets", label: "Datasets", key: "d", group: "Evaluate" },
  { to: "/drift", label: "Drift", key: "f", group: "Evaluate" },
  { to: "/schedules", label: "Schedules", key: "s", group: "Evaluate" },
  { to: "/alerts", label: "Alerts", key: "a", group: "Evaluate" },
  { to: "/settings", label: "Settings", key: ",", group: "Workspace" },
] as const;

const GROUPS = ["Track", "Evaluate", "Workspace"] as const;

/** Counts beside each item; drift and alerts count problems, so they are red when above zero. */
function useCounts(): Record<string, { n: number; bad?: boolean; label: string } | undefined> {
  const meta = useMeta();
  const suites = useQuery({ queryKey: ["suites"], queryFn: ({ signal }) => evalApi.suites(signal), refetchInterval: 60_000 });
  const m = meta.data;
  if (!m) return {};
  const drifting = (suites.data ?? []).filter((s) => s.behaviour.some((b) => b.drifted) || s.data === "drift").length;
  const experiments = m.experiments.filter((e) => e.name).length;
  return {
    "/": { n: m.runs, label: `${integer(m.runs)} runs` },
    "/experiments": { n: experiments, label: `${experiments} experiments` },
    "/traces": m.traces !== undefined ? { n: m.traces, label: `${integer(m.traces)} traces` } : undefined,
    "/datasets": { n: m.datasets, label: `${m.datasets} datasets` },
    "/drift": suites.data ? { n: drifting, bad: drifting > 0, label: `${drifting} drifting` } : undefined,
    "/schedules": { n: m.schedules, label: `${m.schedules} schedules` },
    "/alerts": { n: m.open_alerts, bad: m.open_alerts > 0, label: `${m.open_alerts} open` },
  };
}

function NavItems({ onNavigate }: { onNavigate?: () => void }) {
  const counts = useCounts();
  return (
    <div className="flex flex-col pb-2">
      {GROUPS.map((g) => (
        <div key={g}>
          <h2 className="mx-5 mb-1.5 mt-4 text-[11px] font-bold uppercase tracking-[0.14em] text-faint">{g}</h2>
          <ul>
            {NAV.filter((n) => n.group === g).map(({ to, label }) => {
              const c = counts[to];
              return (
                <li key={to}>
                  <NavLink
                    to={to}
                    end={to === "/"}
                    onClick={onNavigate}
                    className={({ isActive }) =>
                      cn(
                        "flex h-9 items-center justify-between border-l-[3px] px-5 pl-[17px] text-base transition-colors duration-[120ms] pointer-coarse:h-11",
                        isActive ? "border-rule bg-background font-bold text-foreground" : "border-transparent text-muted-foreground hover:bg-hover hover:text-foreground",
                      )
                    }
                  >
                    {label}
                    {c && (c.n > 0 || c.bad !== undefined) && (
                      <span className={cn("font-mono text-xs nums", c.bad ? "font-bold text-danger" : "font-normal text-faint")} aria-label={c.label}>
                        {integer(c.n)}
                      </span>
                    )}
                  </NavLink>
                </li>
              );
            })}
          </ul>
        </div>
      ))}
    </div>
  );
}

function Wordmark() {
  return (
    <Link to="/" className="flex min-h-8 items-center gap-2 text-foreground" translate="no" aria-label="vigily, go to runs">
      <Mark className="h-[19px] w-auto" />
      <WordmarkText className="h-[22px] w-auto" />
    </Link>
  );
}

/** The repository this server reads: the last two parts of its path. */
function Workspace() {
  const meta = useMeta();
  const repo = meta.data?.repository ?? "";
  return (
    <div className="mx-3.5 mb-1.5 mt-3.5 flex items-center justify-between gap-2 border bg-background px-2.5 py-2 text-sm" title={repo}>
      <span className="truncate font-mono text-[12.5px]">{repo ? repo.split("/").slice(-2).join("/") : " "}</span>
      <span className="shrink-0 text-xs text-faint">local</span>
    </div>
  );
}

function SidebarFoot() {
  const meta = useMeta();
  const up = !meta.isError;
  return (
    // A pre-release version ("v0.1.0-beta.1") does not fit beside the status in 232 px: it moves to
    // its own line whole instead of breaking at the hyphen, and only truncates past the full width.
    <div className="mt-auto flex flex-wrap items-center gap-x-3 gap-y-1 border-t px-5 py-3 text-xs text-faint">
      <span className="flex items-center gap-2 whitespace-nowrap">
        <span className={cn("inline-block size-[7px]", up ? "bg-brand" : "bg-danger")} aria-hidden />
        {up ? "Server running" : "Server unreachable"}
      </span>
      {meta.data && (
        <span className="ml-auto min-w-0 max-w-full truncate whitespace-nowrap font-mono nums" title={`vigily ${meta.data.version}`}>
          v{meta.data.version}
        </span>
      )}
    </div>
  );
}

function Breadcrumbs() {
  const trail = useCrumbTrail();
  const location = useLocation();
  const fallback = NAV.find((n) => (n.to === "/" ? location.pathname === "/" : location.pathname.startsWith(n.to)));
  const crumbs = trail ?? [{ label: fallback?.label ?? "vigily" }];
  return (
    <nav aria-label="Breadcrumb" className="min-w-0 flex-1">
      <ol className="flex min-w-0 items-center text-[13.5px] text-faint">
        {crumbs.map((c, i) => {
          const last = i === crumbs.length - 1;
          return (
            <li key={i} className={cn("flex min-w-0 items-center", !last && "max-sm:hidden")}>
              {i > 0 && (
                <span className="mx-2 text-border max-sm:hidden" aria-hidden>
                  /
                </span>
              )}
              {last || !c.to ? (
                <span className={cn("truncate", last && "font-bold text-foreground", c.mono && "font-mono text-[13px]")} aria-current={last ? "page" : undefined}>
                  {c.label}
                </span>
              ) : (
                <Link to={c.to} className={cn("inline-flex min-h-6 items-center truncate hover:text-foreground hover:underline", c.mono && "font-mono text-[13px]")}>
                  {c.label}
                </Link>
              )}
            </li>
          );
        })}
      </ol>
    </nav>
  );
}

export function Layout() {
  return (
    <CrumbsProvider>
      <Shell />
    </CrumbsProvider>
  );
}

function Shell() {
  const { theme, toggle } = useTheme();
  const [palette, setPalette] = useState(false);
  const [shortcuts, setShortcuts] = useState(false);
  const [drawer, setDrawer] = useState(false);
  const navigate = useNavigate();
  const location = useLocation();
  const pendingG = useRef(0);

  // Move focus to the page on navigation, so keyboard users start at the content.
  const main = useRef<HTMLElement>(null);
  const first = useRef(true);
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    main.current?.focus({ preventScroll: true });
    main.current?.scrollTo({ top: 0 });
  }, [location.pathname]);

  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPalette((p) => !p);
        return;
      }
      if (typing(e) || e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.key === "?") {
        e.preventDefault();
        setShortcuts(true);
      } else if (e.key === "g") {
        pendingG.current = Date.now();
      } else if (Date.now() - pendingG.current < 1000) {
        const item = NAV.find((n) => n.key === e.key);
        pendingG.current = 0;
        if (item) {
          e.preventDefault();
          navigate(item.to);
        }
      }
    };
    window.addEventListener("keydown", on);
    return () => window.removeEventListener("keydown", on);
  }, [navigate]);

  const ThemeIcon = theme === "dark" ? Sun : Moon;
  const themeLabel = theme === "dark" ? "Switch to light theme" : "Switch to dark theme";
  const iconButton = "inline-flex size-9 shrink-0 items-center justify-center border border-transparent text-muted-foreground hover:border-border hover:bg-card hover:text-foreground pointer-coarse:size-11";

  return (
    <div className="flex h-full">
      <a href="#main" className="sr-only z-50 bg-popover px-3 py-2 shadow-float focus:not-sr-only focus:fixed focus:left-2 focus:top-2">
        Skip to content
      </a>
      <aside className="hidden w-[232px] shrink-0 flex-col border-r bg-sidebar md:flex">
        <div className="flex h-[52px] shrink-0 items-center border-b px-5">
          <Wordmark />
        </div>
        <Workspace />
        <nav aria-label="Main" className="min-h-0 flex-1 overflow-y-auto">
          <NavItems />
        </nav>
        <SidebarFoot />
      </aside>

      <div className="flex min-w-0 flex-1 flex-col">
        <header className="box-content flex h-[52px] shrink-0 items-center gap-2 border-b bg-background pl-[max(0.5rem,env(safe-area-inset-left))] pr-[max(0.5rem,env(safe-area-inset-right))] pt-[env(safe-area-inset-top)] md:gap-4 md:px-6">
          <button type="button" onClick={() => setDrawer(true)} aria-label="Open menu" className={cn(iconButton, "md:hidden")}>
            <Menu className="size-5" />
          </button>
          <Breadcrumbs />
          <button
            type="button"
            onClick={() => setPalette(true)}
            className="hidden h-9 w-[min(320px,30vw)] items-center justify-between gap-2 border bg-card px-2.5 text-sm text-faint hover:text-foreground sm:flex"
          >
            <span className="truncate">Search runs, metrics, evals</span>
            <Kbd>{modKey === "⌘" ? "⌘K" : "Ctrl K"}</Kbd>
          </button>
          <button type="button" onClick={() => setPalette(true)} aria-label="Search" className={cn(iconButton, "sm:hidden")}>
            <Search className="size-5" />
          </button>
          <a href={DOCS_URL} target="_blank" rel="noreferrer" className="hidden h-9 items-center border bg-card px-3 text-sm font-semibold hover:bg-hover sm:inline-flex">
            Docs
          </a>
          <Tooltip content="Keyboard shortcuts (?)">
            <button type="button" onClick={() => setShortcuts(true)} aria-label="Keyboard shortcuts" className={cn(iconButton, "max-lg:hidden")}>
              <Keyboard className="size-4" strokeWidth={1.75} />
            </button>
          </Tooltip>
          <Tooltip content={themeLabel}>
            <button type="button" onClick={toggle} aria-label={themeLabel} className={iconButton}>
              <ThemeIcon className="size-4" strokeWidth={1.75} />
            </button>
          </Tooltip>
        </header>
        <main id="main" ref={main} tabIndex={-1} className="relative min-h-0 flex-1 overflow-auto outline-none">
          <Outlet />
        </main>
      </div>

      <Dialog open={drawer} onOpenChange={setDrawer}>
        <DialogContent className="left-0 top-0 flex h-full max-h-none w-[272px] max-w-[85vw] translate-x-0 translate-y-0 flex-col gap-0 border-r bg-sidebar p-0 shadow-none">
          <DialogTitle className="flex h-[52px] shrink-0 items-center border-b px-5">
            <Wordmark />
          </DialogTitle>
          <Workspace />
          <nav aria-label="Main" className="min-h-0 flex-1 overflow-y-auto">
            <NavItems onNavigate={() => setDrawer(false)} />
          </nav>
          <a href={DOCS_URL} target="_blank" rel="noreferrer" className="flex h-11 items-center border-t px-5 text-base font-semibold">
            Docs
          </a>
          <SidebarFoot />
        </DialogContent>
      </Dialog>
      <CommandPalette open={palette} onOpenChange={setPalette} onShortcuts={() => setShortcuts(true)} />
      <ShortcutsDialog open={shortcuts} onOpenChange={setShortcuts} />
    </div>
  );
}
