"use client";

import * as React from "react";

import { useLocalStorage } from "@/hooks/use-local-storage";
import { useMediaQuery } from "@/hooks/use-media-query";

/// サイドバーの寸法定数（px）。リサイザのクランプ範囲とレール幅の正本。
export const SIDEBAR_MIN_WIDTH = 240;
export const SIDEBAR_MAX_WIDTH = 480;
export const SIDEBAR_DEFAULT_WIDTH = 288;
export const SIDEBAR_RAIL_WIDTH = 60;
/// この幅未満はモバイル扱い（ドロワ表示）。Tailwind の md(768px) と揃える。
export const SIDEBAR_MOBILE_QUERY = "(max-width: 767px)";

const WIDTH_KEY = "shiki:sidebar:width";
const COLLAPSED_KEY = "shiki:sidebar:collapsed";

export function clampWidth(px: number): number {
  return Math.min(SIDEBAR_MAX_WIDTH, Math.max(SIDEBAR_MIN_WIDTH, Math.round(px)));
}

type SidebarContextValue = {
  isMobile: boolean;
  /// デスクトップでレール（アイコンのみ）に畳んでいるか。
  collapsed: boolean;
  setCollapsed: (v: boolean) => void;
  toggleCollapsed: () => void;
  /// 展開時の幅（px）。モバイルやレール時は描画に使わない。
  width: number;
  setWidth: (px: number) => void;
  resetWidth: () => void;
  /// モバイルのドロワ開閉。
  mobileOpen: boolean;
  setMobileOpen: (v: boolean) => void;
  /// 現在の実効カラム幅（CSS 用の px 文字列）。
  effectiveWidthPx: number;
};

const SidebarContext = React.createContext<SidebarContextValue | null>(null);

export function useSidebar(): SidebarContextValue {
  const ctx = React.useContext(SidebarContext);
  if (!ctx) throw new Error("useSidebar は SidebarProvider 内で使用してください");
  return ctx;
}

export function SidebarProvider({ children }: { children: React.ReactNode }) {
  const isMobile = useMediaQuery(SIDEBAR_MOBILE_QUERY);
  const [collapsed, setCollapsedState] = useLocalStorage<boolean>(COLLAPSED_KEY, false);
  const [storedWidth, setStoredWidth] = useLocalStorage<number>(
    WIDTH_KEY,
    SIDEBAR_DEFAULT_WIDTH,
  );
  const [mobileOpen, setMobileOpen] = React.useState(false);

  // デスクトップ幅へ移行したらモバイルドロワを閉じる。開いたままだと Radix Dialog の
  // focus trap / scroll lock が不可視のコンテンツに残り、デスクトップ操作を妨げる。
  React.useEffect(() => {
    if (!isMobile && mobileOpen) setMobileOpen(false);
  }, [isMobile, mobileOpen]);

  const setWidth = React.useCallback(
    (px: number) => setStoredWidth(clampWidth(px)),
    [setStoredWidth],
  );
  const resetWidth = React.useCallback(
    () => setStoredWidth(SIDEBAR_DEFAULT_WIDTH),
    [setStoredWidth],
  );
  const setCollapsed = React.useCallback(
    (v: boolean) => setCollapsedState(v),
    [setCollapsedState],
  );
  const toggleCollapsed = React.useCallback(
    () => setCollapsedState((p) => !p),
    [setCollapsedState],
  );

  const width = clampWidth(storedWidth);
  const effectiveWidthPx = collapsed ? SIDEBAR_RAIL_WIDTH : width;

  const value = React.useMemo<SidebarContextValue>(
    () => ({
      isMobile,
      collapsed,
      setCollapsed,
      toggleCollapsed,
      width,
      setWidth,
      resetWidth,
      mobileOpen,
      setMobileOpen,
      effectiveWidthPx,
    }),
    [
      isMobile,
      collapsed,
      setCollapsed,
      toggleCollapsed,
      width,
      setWidth,
      resetWidth,
      mobileOpen,
      effectiveWidthPx,
    ],
  );

  return <SidebarContext.Provider value={value}>{children}</SidebarContext.Provider>;
}
