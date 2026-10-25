"use client";

import * as React from "react";
import * as DialogPrimitive from "@radix-ui/react-dialog";
import { X } from "lucide-react";

import { guardLocalEscape } from "@/components/ui/local-escape";
import { useMergedRef, useSwipeToDismiss } from "@/hooks/use-swipe-to-dismiss";
import { cn } from "@/lib/utils";

const Dialog = DialogPrimitive.Root;
const DialogTrigger = DialogPrimitive.Trigger;
const DialogClose = DialogPrimitive.Close;
const DialogPortal = DialogPrimitive.Portal;

function DialogTitle({
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Title>) {
  return (
    <DialogPrimitive.Title
      className={cn("text-lg font-semibold leading-none tracking-tight", className)}
      {...props}
    />
  );
}

function DialogDescription({
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Description>) {
  return (
    <DialogPrimitive.Description
      className={cn("text-sm text-muted-foreground", className)}
      {...props}
    />
  );
}

function DialogOverlay({
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Overlay>) {
  return (
    <DialogPrimitive.Overlay
      data-slot="dialog-overlay"
      className={cn(
        "fixed inset-0 z-50 bg-black/40 backdrop-blur-[1px]",
        "data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=closed]:animate-out data-[state=closed]:fade-out-0",
        className,
      )}
      {...props}
    />
  );
}

/// スマホ幅（sm 未満）でだけ下からのシートになる。そのときだけ下へ払って閉じられる。
const SHEET_QUERY = "(max-width: 639.98px)";

function DialogContent({
  className,
  children,
  showClose = true,
  ref,
  onEscapeKeyDown,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Content> & { showClose?: boolean }) {
  const closeRef = React.useRef<HTMLButtonElement>(null);
  const swipeRef = useSwipeToDismiss<HTMLDivElement>({
    enabled: () => window.matchMedia(SHEET_QUERY).matches,
    close: () => closeRef.current?.click(),
  });
  const contentRef = useMergedRef(ref, swipeRef);
  return (
    <DialogPortal>
      <DialogOverlay />
      <DialogPrimitive.Content
        ref={contentRef}
        data-slot="dialog-content"
        className={cn(
          "fixed z-50 grid gap-4 border border-border bg-card p-6 text-card-foreground shadow-lg",
          "overflow-y-auto overscroll-contain outline-none",
          // sm 以上: 中央のダイアログ。画面端に密着させず、縦は画面内に収めて内側でスクロールする。
          // 幅（w / max-w）は接頭辞なしで置く。呼び出し側の `max-w-2xl` 等を tailwind-merge で
          // 上書きできるようにするため（`sm:max-w-lg` だと別バリアント扱いで呼び出し側が負ける）。
          "w-[calc(100%-2rem)] max-w-lg",
          "sm:left-1/2 sm:top-1/2 sm:-translate-x-1/2 sm:-translate-y-1/2 sm:max-h-[calc(100dvh-2rem)] sm:rounded-xl",
          "data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=closed]:animate-out data-[state=closed]:fade-out-0",
          "sm:data-[state=open]:zoom-in-95 sm:data-[state=closed]:zoom-out-95",
          // スマホ: 親指の届く下からのシート。キーボードが出ても dvh で縮み、ホームバーを避ける。
          // 呼び出し側の接頭辞なし max-w-* は max-sm バリアント（後段に出力される）で打ち消し、常に全幅にする。
          "max-sm:inset-x-0 max-sm:bottom-0 max-sm:w-full max-sm:max-w-none max-sm:max-h-[92dvh] max-sm:rounded-t-2xl max-sm:border-x-0 max-sm:border-b-0 max-sm:px-5 max-sm:pt-3 max-sm:pb-[max(1.25rem,env(safe-area-inset-bottom))]",
          "max-sm:data-[state=open]:slide-in-from-bottom max-sm:data-[state=closed]:slide-out-to-bottom",
          className,
        )}
        {...props}
        // 中の候補一覧（参照の選択など）が自分で閉じる Esc ではダイアログを閉じない（2-A-P2-2）。
        onEscapeKeyDown={guardLocalEscape(onEscapeKeyDown)}
      >
        <div aria-hidden className="mx-auto h-1.5 w-9 rounded-full bg-muted-foreground/25 sm:hidden" />
        {children}
        {showClose ? (
          <DialogPrimitive.Close
            ref={closeRef}
            aria-label="閉じる"
            className="absolute right-2.5 top-2.5 flex size-9 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:size-11 max-sm:top-4"
          >
            <X className="size-4" aria-hidden />
          </DialogPrimitive.Close>
        ) : (
          // 閉じるボタンを出さない画面でも、スワイプで閉じるための押し先を置く（見えない・Tab で来ない）。
          <DialogPrimitive.Close ref={closeRef} tabIndex={-1} aria-hidden className="sr-only" />
        )}
      </DialogPrimitive.Content>
    </DialogPortal>
  );
}

function DialogHeader({ className, ...props }: React.ComponentProps<"div">) {
  return (
    // 右上の閉じるボタンに長いタイトルが重ならないよう右側を空ける。
    <div className={cn("flex flex-col gap-1.5 pr-10 text-left", className)} {...props} />
  );
}

function DialogFooter({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      className={cn("flex flex-col-reverse gap-2 sm:flex-row sm:justify-end", className)}
      {...props}
    />
  );
}

export {
  Dialog,
  DialogTrigger,
  DialogClose,
  DialogPortal,
  DialogOverlay,
  DialogContent,
  DialogHeader,
  DialogFooter,
  DialogTitle,
  DialogDescription,
};
