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
  /// デスクトップでレール（アイコンのみ）に畳んでいるか（＝手動 pref ∪ ルート由来の没入）。
  collapsed: boolean;
  setCollapsed: (v: boolean) => void;
  toggleCollapsed: () => void;
  /// 没入エディタ（ルート由来）の一時折りたたみを設定する。手動 pref（localStorage）は
  /// 汚さない。エディタを離れると自動で手動 pref の状態へ戻る（AppShell が制御）。
  setRouteImmersive: (on: boolean) => void;
  /// 展開時の幅（px）。モバイルやレール時は描画に使わない。
  width: number;
  setWidth: (px: number) => void;
  resetWidth: () => void;
  /// モバイルのドロワ開閉。
  mobileOpen: boolean;
  setMobileOpen: (v: boolean) => void;
  /// 検索パレットの開閉。シェルで単一インスタンスを持ち、デスクトップ/モバイル
  /// 双方のナビから同じダイアログを開く（多重マウントによる二重ショートカットを防ぐ）。
  searchOpen: boolean;
  setSearchOpen: (v: boolean) => void;
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
  // 手動 pref（永続）。実効 collapsed はこれとルート由来没入の論理和。
  const [userCollapsed, setCollapsedState] = useLocalStorage<boolean>(COLLAPSED_KEY, false);
  // ルート由来の一時折りたたみ（永続しない・エディタ滞在中のみ true）。
  const [routeImmersive, setRouteImmersiveState] = React.useState(false);
  const collapsed = userCollapsed || routeImmersive;
  const [storedWidth, setStoredWidth] = useLocalStorage<number>(
    WIDTH_KEY,
    SIDEBAR_DEFAULT_WIDTH,
  );
  const [mobileOpen, setMobileOpen] = React.useState(false);
  const [searchOpen, setSearchOpen] = React.useState(false);

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
  // 手動で開くときはルート由来の没入も解除する（エディタ内でも一時的にサイドバーを覗ける）。
  const setCollapsed = React.useCallback(
    (v: boolean) => {
      setCollapsedState(v);
      if (!v) setRouteImmersiveState(false);
    },
    [setCollapsedState],
  );
  const toggleCollapsed = React.useCallback(
    () => setCollapsed(!(userCollapsed || routeImmersive)),
    [setCollapsed, userCollapsed, routeImmersive],
  );
  const setRouteImmersive = React.useCallback(
    (on: boolean) => setRouteImmersiveState(on),
    [],
  );

  const width = clampWidth(storedWidth);
  const effectiveWidthPx = collapsed ? SIDEBAR_RAIL_WIDTH : width;

  const value = React.useMemo<SidebarContextValue>(
    () => ({
      isMobile,
      collapsed,
      setCollapsed,
      toggleCollapsed,
      setRouteImmersive,
      width,
      setWidth,
      resetWidth,
      mobileOpen,
      setMobileOpen,
      searchOpen,
      setSearchOpen,
      effectiveWidthPx,
    }),
    [
      isMobile,
      collapsed,
      setCollapsed,
      toggleCollapsed,
      setRouteImmersive,
      width,
      setWidth,
      resetWidth,
      mobileOpen,
      searchOpen,
      effectiveWidthPx,
    ],
  );

  return <SidebarContext.Provider value={value}>{children}</SidebarContext.Provider>;
}
