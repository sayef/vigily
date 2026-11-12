import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter, Link, Route, Routes } from "react-router";
import { Toaster } from "sonner";
import { Layout } from "@/components/layout";
import { AuthGate } from "@/components/sign-in";
import { ThemeProvider, useTheme } from "@/components/theme";
import { TooltipProvider } from "@/components/ui/tooltip";
import { SelectionProvider } from "@/lib/selection";
import { AlertsPage } from "@/pages/alerts";
import { ComparePage } from "@/pages/compare";
import { CompareRunsPage } from "@/pages/compare-runs";
import { DatasetPage, DatasetsPage } from "@/pages/datasets";
import { DriftPage, SuitePage } from "@/pages/drift";
import { ExperimentsPage } from "@/pages/experiments";
import { SchedulePage, SchedulesPage } from "@/pages/schedules";
import { ExplorerPage } from "@/pages/explorer";
import { RunPage } from "@/pages/run";
import { RunsPage } from "@/pages/runs";
import { SettingsPage } from "@/pages/settings";
import { TracePage } from "@/pages/trace";
import { TracesPage } from "@/pages/traces";
import "./index.css";

const queries = new QueryClient({
  defaultOptions: { queries: { staleTime: 2_000, refetchOnWindowFocus: true } },
});

function Toasts() {
  const { theme } = useTheme();
  return (
    <Toaster
      theme={theme}
      position="bottom-right"
      toastOptions={{ className: "bg-popover! text-foreground! border-border! shadow-float! font-sans!" }}
    />
  );
}

function NotFound() {
  return (
    <div className="px-4 pt-5 md:px-6">
      <h1 className="border-b-[1.5px] border-rule pb-4 text-xl font-bold tracking-[-0.02em]">No page here</h1>
      <p className="mt-3 text-muted-foreground">This address does not match a vigily page.</p>
      <Link to="/" className="mt-3 inline-block font-semibold underline underline-offset-4">
        Go to runs
      </Link>
    </div>
  );
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ThemeProvider>
      <QueryClientProvider client={queries}>
        <TooltipProvider delayDuration={300} skipDelayDuration={200}>
          <SelectionProvider>
            <AuthGate>
              <BrowserRouter>
                <Routes>
                  <Route element={<Layout />}>
                    <Route index element={<RunsPage />} />
                    <Route path="runs/:id" element={<RunPage />} />
                    <Route path="metrics" element={<ExplorerPage />} />
                    <Route path="metrics/compare" element={<CompareRunsPage />} />
                    <Route path="experiments" element={<ExperimentsPage />} />
                    <Route path="traces" element={<TracesPage />} />
                    <Route path="traces/:id" element={<TracePage />} />
                    <Route path="datasets" element={<DatasetsPage />} />
                    <Route path="datasets/:name" element={<DatasetPage />} />
                    <Route path="compare" element={<ComparePage />} />
                    <Route path="schedules" element={<SchedulesPage />} />
                    <Route path="schedules/:name" element={<SchedulePage />} />
                    <Route path="drift" element={<DriftPage />} />
                    <Route path="drift/:key" element={<SuitePage />} />
                    <Route path="alerts" element={<AlertsPage />} />
                    <Route path="settings" element={<SettingsPage />} />
                    <Route path="*" element={<NotFound />} />
                  </Route>
                </Routes>
              </BrowserRouter>
            </AuthGate>
            <Toasts />
          </SelectionProvider>
        </TooltipProvider>
      </QueryClientProvider>
    </ThemeProvider>
  </StrictMode>,
);
