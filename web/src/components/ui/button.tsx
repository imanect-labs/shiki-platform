"use client";

import * as React from "react";
import { Slot } from "@radix-ui/react-slot";
import { cva, type VariantProps } from "class-variance-authority";
import { Loader2 } from "lucide-react";

import { cn } from "@/lib/utils";

const buttonVariants = cva(
  cn(
    "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm font-medium",
    // 色に加えて transform も遷移（押下の active:scale を滑らかに）。動かすのは transform/opacity のみ。
    "transition-[color,background-color,border-color,box-shadow,transform] duration-[var(--duration-fast)] ease-[var(--ease-standard)]",
    "active:scale-[0.97]",
    "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
    "disabled:pointer-events-none disabled:opacity-50",
    "[&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*=size-])]:size-4",
  ),
  {
    variants: {
      variant: {
        default: "bg-primary text-primary-foreground shadow-xs hover:bg-primary/90",
        secondary: "bg-secondary text-secondary-foreground hover:bg-secondary/80",
        outline:
          "border border-border bg-background shadow-xs hover:bg-accent hover:text-accent-foreground",
        ghost: "hover:bg-accent hover:text-accent-foreground",
        destructive:
          "bg-destructive text-destructive-foreground shadow-xs hover:bg-destructive/90",
        link: "text-primary underline-offset-4 hover:underline",
      },
      size: {
        sm: "h-8 rounded-md px-3 text-xs",
        default: "h-9 px-4 py-2",
        lg: "h-10 rounded-md px-6",
        icon: "size-9",
      },
    },
    defaultVariants: { variant: "default", size: "default" },
  },
);

type ButtonProps = React.ComponentProps<"button"> &
  VariantProps<typeof buttonVariants> & {
    /// true の場合は子要素（例: Link）をボタンとして描画する（多態）。
    asChild?: boolean;
    /// 処理中スピナを先頭に表示し、操作を無効化する（呼び出し側のスピナ手書きを不要にする）。
    /// asChild（Link 等）とは併用しない（子要素の描画を横取りしないため無視する）。
    loading?: boolean;
  };

function Button({
  className,
  variant,
  size,
  asChild = false,
  loading = false,
  disabled,
  children,
  ...props
}: ButtonProps) {
  const Comp = asChild ? Slot : "button";
  // 通常の <button> はフォーム内で暗黙 type="submit" になり意図しない送信を起こす。
  // 明示指定が無ければ type="button" を既定にする（asChild では子要素に委ねる）。
  const type = asChild ? props.type : (props.type ?? "button");
  // loading は多態（asChild）では意味を持たない（子の中身を差し替えられない）ため無視。
  const showSpinner = loading && !asChild;
  return (
    <Comp
      data-slot="button"
      className={cn(buttonVariants({ variant, size }), className)}
      aria-busy={showSpinner || undefined}
      disabled={asChild ? disabled : disabled || showSpinner}
      {...props}
      type={type}
    >
      {/* asChild(Slot) は単一子要素を要求するため、spinner が無い場合は children を素で渡す。 */}
      {showSpinner ? (
        <>
          <Loader2 className="animate-spin" aria-hidden />
          {children}
        </>
      ) : (
        children
      )}
    </Comp>
  );
}

export { Button, buttonVariants };
export type { ButtonProps };
