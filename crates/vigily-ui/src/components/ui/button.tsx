import { Slot } from "@radix-ui/react-slot";
import { cva, type VariantProps } from "class-variance-authority";
import { Loader2 } from "lucide-react";
import * as React from "react";
import { cn } from "@/lib/utils";

export const buttonVariants = cva(
  "inline-flex shrink-0 items-center justify-center gap-1.5 whitespace-nowrap text-sm font-semibold transition-[background-color,color] duration-[120ms] ease-out disabled:pointer-events-none disabled:opacity-45 [&_svg]:size-4 [&_svg]:shrink-0",
  {
    variants: {
      variant: {
        brand: "border border-foreground bg-foreground text-background hover:bg-[color-mix(in_srgb,var(--color-foreground)_82%,var(--color-background))]",
        secondary: "border border-border bg-card text-foreground hover:bg-hover",
        ghost: "text-muted-foreground hover:bg-hover hover:text-foreground",
        outline: "border border-border bg-card text-foreground hover:bg-hover",
        danger: "bg-danger text-[var(--card)] hover:bg-[color-mix(in_srgb,var(--color-danger)_85%,var(--color-foreground))]",
      },
      size: {
        default: "h-9 px-3.5 pointer-coarse:h-11",
        sm: "h-7 px-2.5 text-xs pointer-coarse:h-11",
        icon: "size-8 pointer-coarse:size-11",
        "icon-sm": "size-7 pointer-coarse:size-11",
      },
    },
    defaultVariants: { variant: "secondary", size: "default" },
  },
);

export interface ButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement>, VariantProps<typeof buttonVariants> {
  asChild?: boolean;
  /** Something this button started is running: a spinner beside the label, further clicks refused. */
  loading?: boolean;
}

export const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
  ({ className, variant, size, asChild = false, loading = false, disabled, children, type, ...props }, ref) => {
    if (asChild)
      return (
        <Slot className={cn(buttonVariants({ variant, size, className }))} ref={ref} {...props}>
          {children}
        </Slot>
      );
    return (
      <button
        type={type ?? "button"}
        className={cn(buttonVariants({ variant, size, className }))}
        ref={ref}
        disabled={disabled || loading}
        aria-busy={loading || undefined}
        {...props}
      >
        {loading && <Loader2 className="animate-spin" aria-hidden />}
        {children}
      </button>
    );
  },
);
Button.displayName = "Button";
