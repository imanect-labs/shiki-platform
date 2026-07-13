"use client";

import type { ReactNode } from "react";
import { ThemeProvider } from "next-themes";
import { MotionConfig } from "motion/react";

import { TooltipProvider } from "@/components/ui/tooltip";
import { ToastProvider } from "@/components/ui/toast";
import { Toaster } from "@/components/ui/toaster";

/// アプリ全体のクライアント側プロバイダ群。
/// - next-themes: system 連動＋手動切替＋FOUC 回避（pre-hydration script を内蔵）。
/// - Radix Tooltip/Toast: provider を 1 度だけ設置し、各所はトリガのみ置く。
/// - MotionConfig: framer-motion 由来の全モーションを OS の「視差効果を減らす」設定に従わせる
///   （reducedMotion="user"）。CSS 側の @media セーフティ（globals.css）と二重で担保する。
export function Providers({ children }: { children: ReactNode }) {
  return (
    <ThemeProvider attribute="class" defaultTheme="system" enableSystem disableTransitionOnChange>
      <MotionConfig reducedMotion="user">
        <ToastProvider swipeDirection="right">
          <TooltipProvider delayDuration={200}>{children}</TooltipProvider>
          <Toaster />
        </ToastProvider>
      </MotionConfig>
    </ThemeProvider>
  );
}
