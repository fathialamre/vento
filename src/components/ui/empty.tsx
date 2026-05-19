import * as React from "react";
import { cn } from "@/lib/utils";

function Empty({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      className={cn(
        "flex flex-col items-center justify-center gap-6 rounded-lg p-8 text-center",
        className,
      )}
      {...props}
    />
  );
}

function EmptyHeader({ className, ...props }: React.ComponentProps<"div">) {
  return <div className={cn("flex flex-col items-center gap-3", className)} {...props} />;
}

function EmptyMedia({
  className,
  variant = "icon",
  ...props
}: React.ComponentProps<"div"> & { variant?: "icon" | "image" }) {
  return (
    <div
      className={cn(
        variant === "icon" &&
          "flex size-12 items-center justify-center rounded-full bg-muted text-muted-foreground [&_svg]:size-6",
        className,
      )}
      {...props}
    />
  );
}

function EmptyTitle({ className, ...props }: React.ComponentProps<"h3">) {
  return (
    <h3 className={cn("text-base font-semibold text-foreground", className)} {...props} />
  );
}

function EmptyDescription({ className, ...props }: React.ComponentProps<"p">) {
  return (
    <p className={cn("text-sm text-muted-foreground", className)} {...props} />
  );
}

function EmptyContent({ className, ...props }: React.ComponentProps<"div">) {
  return <div className={cn("flex flex-col items-center gap-2", className)} {...props} />;
}

export { Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle };
