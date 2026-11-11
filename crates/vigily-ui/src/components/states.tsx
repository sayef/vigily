import { RefreshCw, TriangleAlert } from "lucide-react";
import { Button } from "@/components/ui/button";

/** What to do when the repository has no runs: the three lines that log one. */
export function EmptyRepository({ repository }: { repository?: string }) {
  return (
    <div className="mx-auto max-w-2xl px-4 py-12">
      <h2 className="text-xl font-bold tracking-[-0.01em]">No runs yet</h2>
      <p className="mt-2 text-base text-muted-foreground">
        Runs appear here as soon as a training script logs one. Install the SDK, add these lines to your training loop, and this page fills in while it runs.
      </p>
      <pre className="mt-4 overflow-x-auto p-4 font-mono text-sm leading-relaxed">
        <code>{`from vigily import Run

run = Run(experiment="mnist")
run["hparams"] = {"lr": 3e-4, "batch_size": 64}
for step, batch in enumerate(loader):
    loss = train_step(batch)
    run.track(loss, name="loss", step=step, context={"subset": "train"})
run.finish()`}</code>
      </pre>
      <p className="mt-4 text-sm text-muted-foreground">
        To see the app with data first, run <code className="rounded-sm bg-muted px-1 text-foreground">python examples/demo.py</code> from the vigily repository.
        {repository && (
          <>
            {" "}
            This server reads <code className="break-all rounded-sm bg-muted px-1 text-foreground">{repository}</code>; scripts write there when run from the same directory or with{" "}
            <code className="rounded-sm bg-muted px-1 text-foreground">VIGILY_REPO</code> set.
          </>
        )}
      </p>
    </div>
  );
}

/** A failed request: what happened and how to try again. */
export function ErrorState({ error, onRetry, className }: { error: Error; onRetry?: () => void; className?: string }) {
  return (
    <div role="alert" className={className ?? "mx-auto flex max-w-xl flex-col items-start gap-3 px-4 py-12"}>
      <h2 className="flex items-center gap-2 text-[17px] font-bold tracking-[-0.01em]">
        <TriangleAlert className="size-5 text-danger" aria-hidden /> Could not load this page
      </h2>
      <p className="text-base text-muted-foreground">{error.message}</p>
      {onRetry && (
        <Button onClick={onRetry}>
          <RefreshCw /> Try again
        </Button>
      )}
    </div>
  );
}
