"use client";

import type { ReactNode } from "react";
import { ThemeProvider } from "next-themes";

import { TooltipProvider } from "@/components/ui/tooltip";
import { ToastProvider } from "@/components/ui/toast";
import { Toaster } from "@/components/ui/toaster";

/// アプリ全体のクライアント側プロバイダ群。
/// - next-themes: system 連動＋手動切替＋FOUC 回避（pre-hydration script を内蔵）。
/// - Radix Tooltip/Toast: provider を 1 度だけ設置し、各所はトリガのみ置く。
export function Providers({ children }: { children: ReactNode }) {
  return (
    <ThemeProvider attribute="class" defaultTheme="system" enableSystem disableTransitionOnChange>
      <ToastProvider swipeDirection="right">
        <TooltipProvider delayDuration={200}>{children}</TooltipProvider>
        <Toaster />
      </ToastProvider>
    </ThemeProvider>
  );
}
