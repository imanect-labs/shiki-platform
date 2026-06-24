"use client";

import {
  Toast,
  ToastClose,
  ToastDescription,
  ToastTitle,
  ToastViewport,
} from "@/components/ui/toast";
import { setToastOpen, useToast } from "@/components/ui/use-toast";

/// ストアのトーストを Radix Toast へ描画する。Providers の ToastProvider 配下に置く。
export function Toaster() {
  const toasts = useToast();

  return (
    <>
      {toasts.map(({ id, title, description, variant, duration, open }) => (
        <Toast
          key={id}
          variant={variant}
          open={open}
          duration={duration ?? 5000}
          onOpenChange={(next) => setToastOpen(id, next)}
        >
          <div className="grid gap-1">
            {title ? <ToastTitle>{title}</ToastTitle> : null}
            {description ? <ToastDescription>{description}</ToastDescription> : null}
          </div>
          <ToastClose />
        </Toast>
      ))}
      <ToastViewport />
    </>
  );
}
