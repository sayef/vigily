import { cn } from "@/lib/utils";

/** A placeholder with the shape of the content it stands for. */
export function Skeleton({ className }: { className?: string }) {
  return <div aria-hidden className={cn("rounded-sm bg-muted motion-safe:animate-pulse", className)} />;
}
