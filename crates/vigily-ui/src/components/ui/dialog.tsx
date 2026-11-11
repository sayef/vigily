import * as DialogPrimitive from "@radix-ui/react-dialog";
import * as React from "react";
import { cn } from "@/lib/utils";

export const Dialog = DialogPrimitive.Root;
export const DialogTitle = DialogPrimitive.Title;
export const DialogDescription = DialogPrimitive.Description;
export const DialogClose = DialogPrimitive.Close;

export function DialogContent({ className, children, ...props }: React.ComponentProps<typeof DialogPrimitive.Content>) {
  return (
    <DialogPrimitive.Portal>
      <DialogPrimitive.Overlay className="fixed inset-0 z-50 bg-[var(--scrim)] data-[state=open]:animate-[fade-in_160ms_var(--ease-out)]" />
      <DialogPrimitive.Content
        className={cn(
          "fixed left-1/2 top-[12%] z-50 max-h-[76vh] w-[calc(100%-2rem)] max-w-xl -translate-x-1/2 overflow-hidden overscroll-contain rounded-lg bg-popover shadow-float outline-none",
          className,
        )}
        aria-describedby={undefined}
        {...props}
      >
        {children}
      </DialogPrimitive.Content>
    </DialogPrimitive.Portal>
  );
}
