import { cn } from "@/lib/utils";

export function Kbd({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    <kbd className={cn("inline-flex h-5 min-w-5 items-center justify-center border border-border px-1 font-mono text-[11px] font-semibold text-muted-foreground", className)}>
      {children}
    </kbd>
  );
}
