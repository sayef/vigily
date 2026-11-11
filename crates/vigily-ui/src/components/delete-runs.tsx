import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Trash2 } from "lucide-react";
import { useCallback, useState, useSyncExternalStore } from "react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import { api } from "@/lib/api";

/** How long a deletion can be undone before it is sent to the server. */
const UNDO_MS = 8_000;

// Runs waiting out their undo window, hidden from every list meanwhile. Module state, so a
// confirmed deletion still completes when the page that started it unmounts.
let hidden = new Set<string>();
const listeners = new Set<() => void>();
const emit = () => listeners.forEach((l) => l());
const setHidden = (next: Set<string>) => {
  hidden = next;
  emit();
};
const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => listeners.delete(l);
};

// The confirmed deletion still in its undo window, if any. There is never more than one: a new
// deletion sends the pending one first. Leaving the page sends it at once (a beacon survives the
// page closing); the user confirmed, and the window only exists for Undo.
let pending: { ids: string[]; timer: number; commit: () => Promise<void> } | null = null;
if (typeof window !== "undefined") {
  window.addEventListener("pagehide", () => {
    if (pending) sendNow(pending.ids);
    pending = null;
  });
}

/** The most a deletion with an undo window may hold: beacons and keepalive requests share about
 * 64 KiB per page, and 1 000 ids fit with room to spare. */
const CHUNK = 1000;
/** The most ids the server takes in one request. */
const MAX_REQUEST = 10_000;

/** Sends a pending deletion while the page goes away: a beacon, or a keepalive fetch when the
 * browser refuses the beacon. It holds at most CHUNK ids, so one request carries it. */
function sendNow(ids: string[]) {
  const body = JSON.stringify({ ids });
  const queued = navigator.sendBeacon?.("/api/runs/delete", new Blob([body], { type: "application/json" }));
  if (!queued) {
    void fetch("/api/runs/delete", { method: "POST", body, headers: { "content-type": "application/json" }, keepalive: true }).catch(() => {});
  }
}

/** Ids of runs deleted in this session whose undo window is still open. */
export function useHiddenRuns(): Set<string> {
  return useSyncExternalStore(subscribe, () => hidden);
}

const plural = (n: number) => `${n} ${n === 1 ? "run" : "runs"}`;

/**
 * The Delete action for selected runs: a confirmation, then an undo window before anything is
 * deleted. Running runs cannot be deleted (their script would keep writing to them), so the
 * action is disabled while the selection contains one, and says why.
 */
export function DeleteRuns({ ids, onDeleted }: { ids: string[]; onDeleted: () => void }) {
  const qc = useQueryClient();
  const [open, setOpen] = useState(false);
  // The selection can hold runs the current filter hides, so ask for every running run.
  const live = useQuery({
    queryKey: ["runs", "status:running", null, false, true],
    queryFn: ({ signal }) => api.runs({ q: "status:running", archived: true }, signal),
    refetchInterval: 5_000,
  });
  const runningIds = new Set(live.data?.rows.map((r) => r.id) ?? []);
  const staleIds = new Set(live.data?.rows.filter((r) => r.stale).map((r) => r.id) ?? []);
  const running = ids.filter((id) => runningIds.has(id)).length;
  const stale = ids.filter((id) => staleIds.has(id)).length;

  const confirm = useCallback(() => {
    const batch = [...ids];
    setOpen(false);
    onDeleted();
    setHidden(new Set([...hidden, ...batch]));
    const restore = () => setHidden(new Set([...hidden].filter((id) => !batch.includes(id))));
    const commit = async () => {
      try {
        // One request up to the server's limit (10 000 ids); a larger selection takes several,
        // and closing the tab while they run leaves the rest undeleted (they reappear).
        for (let i = 0; i < batch.length; i += MAX_REQUEST) await api.deleteRuns(batch.slice(i, i + MAX_REQUEST));
      } catch (e) {
        toast.error(`${plural(batch.length)} could not all be deleted: ${(e as Error).message}`);
      } finally {
        restore();
        await qc.invalidateQueries();
      }
    };
    // Only one deletion waits at a time: send the one already waiting now.
    if (pending) {
      const p = pending;
      pending = null;
      window.clearTimeout(p.timer);
      void p.commit();
    }
    // More than CHUNK ids is too much to send while the page closes, so a large deletion goes
    // at once, without Undo (the dialog says so).
    if (batch.length > CHUNK) {
      toast(`Deleting ${plural(batch.length)}`);
      void commit();
      return;
    }
    const timer = window.setTimeout(() => {
      if (pending?.timer === timer) {
        pending = null;
        void commit();
      }
    }, UNDO_MS);
    pending = { ids: batch, timer, commit };
    toast(`Deletion pending: ${plural(batch.length)}`, {
      description: `Deleted in ${UNDO_MS / 1000} seconds unless you undo. Leaving the page deletes ${batch.length === 1 ? "it" : "them"} at once.`,
      duration: UNDO_MS,
      action: {
        label: "Undo",
        onClick: () => {
          // A later deletion may already have sent this one.
          if (pending?.timer !== timer) {
            toast.error(`Too late to undo: ${plural(batch.length)} already deleted.`);
            return;
          }
          pending = null;
          window.clearTimeout(timer);
          restore();
        },
      },
    });
  }, [ids, onDeleted, qc]);

  return (
    <>
      {running > 0 && (
        <span className="order-last w-full text-xs text-muted-foreground sm:order-none sm:w-auto" id="delete-runs-why">
          {running === 1 ? "1 selected run is" : `${running} selected runs are`} still running; stop {running === 1 ? "it" : "them"} to delete.
          {stale > 0 && ` ${stale === 1 ? "One has" : `${stale} have`} no heartbeat: mark ${stale === 1 ? "it" : "them"} as aborted on the run page.`}
        </span>
      )}
      <Button variant="ghost" size="sm" onClick={() => setOpen(true)} disabled={running > 0 || !live.data} aria-describedby={running > 0 ? "delete-runs-why" : undefined}>
        <Trash2 /> Delete
      </Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="max-w-md p-5">
          <DialogTitle className="text-lg font-semibold">Delete {plural(ids.length)}?</DialogTitle>
          <DialogDescription className="mt-2 text-sm text-muted-foreground">
            Their parameters, metric series and evaluation results are deleted with them. {ids.length > CHUNK ? "There is no undo for more than 1,000 runs at once." : "You can undo for a few seconds afterwards; after that, deleting cannot be reversed."} Older storage versions keep the data until they are cleaned up.
          </DialogDescription>
          <div className="mt-5 flex justify-end gap-2">
            <DialogClose asChild>
              <Button variant="secondary">Cancel</Button>
            </DialogClose>
            <Button variant="danger" onClick={confirm}>
              <Trash2 /> Delete {plural(ids.length)}
            </Button>
          </div>
        </DialogContent>
      </Dialog>
    </>
  );
}
