import * as React from "react";
import { cn } from "@/lib/utils";

export const Input = React.forwardRef<HTMLInputElement, React.InputHTMLAttributes<HTMLInputElement>>(({ className, ...props }, ref) => (
  <input
    ref={ref}
    className={cn(
      "h-8 w-full min-w-0 rounded-md border border-input bg-card px-2.5 text-sm text-foreground placeholder:text-muted-foreground focus-visible:outline-2 pointer-coarse:h-11",
      className,
    )}
    {...props}
  />
));
Input.displayName = "Input";
