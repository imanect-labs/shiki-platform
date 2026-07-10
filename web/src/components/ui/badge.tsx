import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";

import { cn } from "@/lib/utils";

const badgeVariants = cva(
  "inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-xs font-medium leading-4 whitespace-nowrap",
  {
    variants: {
      variant: {
        default: "border-transparent bg-primary text-primary-foreground",
        secondary: "border-transparent bg-secondary text-secondary-foreground",
        outline: "border-border text-foreground",
        success:
          "border-transparent bg-[oklch(0.93_0.06_150)] text-[oklch(0.35_0.09_150)] dark:bg-[oklch(0.3_0.06_150)] dark:text-[oklch(0.85_0.08_150)]",
        warning:
          "border-transparent bg-[oklch(0.94_0.06_80)] text-[oklch(0.4_0.1_70)] dark:bg-[oklch(0.32_0.06_80)] dark:text-[oklch(0.87_0.09_85)]",
        destructive:
          "border-transparent bg-[oklch(0.93_0.05_25)] text-[oklch(0.4_0.14_25)] dark:bg-[oklch(0.3_0.08_25)] dark:text-[oklch(0.85_0.09_25)]",
        muted: "border-transparent bg-muted text-muted-foreground",
      },
    },
    defaultVariants: { variant: "default" },
  },
);

function Badge({
  className,
  variant,
  ...props
}: React.ComponentProps<"span"> & VariantProps<typeof badgeVariants>) {
  return <span className={cn(badgeVariants({ variant }), className)} {...props} />;
}

export { Badge, badgeVariants };
