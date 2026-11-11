import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { Kbd } from "@/components/ui/kbd";
import { modKey } from "@/lib/utils";

const GROUPS: { title: string; keys: [string[], string][] }[] = [
  {
    title: "Anywhere",
    keys: [
      [[modKey, "K"], "Search runs, experiments, metrics and actions"],
      [["g", "r"], "Go to runs"],
      [["g", "m"], "Go to metrics"],
      [["g", "e"], "Go to experiments"],
      [["g", "t"], "Go to traces"],
      [["g", "d"], "Go to datasets"],
      [["g", "f"], "Go to drift"],
      [["g", "s"], "Go to schedules"],
      [["g", "a"], "Go to alerts"],
      [["g", ","], "Go to settings"],
      [["?"], "Show these shortcuts"],
    ],
  },
  {
    title: "Runs",
    keys: [
      [["/"], "Search and filter runs"],
      [["j"], "Next run"],
      [["k"], "Previous run"],
      [["x"], "Select or unselect the run"],
      [["Esc"], "Clear the selection"],
      [["Enter"], "Open the run"],
      [["c"], "Compare the selected runs"],
    ],
  },
  {
    title: "Traces",
    keys: [
      [["/"], "Find a trace"],
      [["j", "k"], "Next or previous trace, or span in a trace"],
      [["Enter"], "Open the trace"],
    ],
  },
  {
    title: "Charts",
    keys: [
      [["←", "→"], "Move the crosshair (Shift for bigger steps)"],
      [["Home", "End"], "First and last point"],
      [["Esc"], "Reset the zoom"],
    ],
  },
];

export function ShortcutsDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (o: boolean) => void }) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg overflow-y-auto p-5">
        <DialogTitle className="text-lg font-semibold">Keyboard shortcuts</DialogTitle>
        <div className="mt-3 flex flex-col gap-5">
          {GROUPS.map((g) => (
            <section key={g.title}>
              <h3 className="mb-1.5 text-sm font-semibold text-muted-foreground">{g.title}</h3>
              <dl className="grid grid-cols-[auto_1fr] items-center gap-x-4 gap-y-1.5">
                {g.keys.map(([keys, label]) => (
                  <div key={label} className="contents">
                    <dt className="flex gap-1">
                      {keys.map((k) => (
                        <Kbd key={k}>{k}</Kbd>
                      ))}
                    </dt>
                    <dd className="text-sm">{label}</dd>
                  </div>
                ))}
              </dl>
            </section>
          ))}
        </div>
      </DialogContent>
    </Dialog>
  );
}
