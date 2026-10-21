"use client";

import * as React from "react";
import Link from "next/link";
import { PanelLeftClose, PanelLeftOpen, X } from "lucide-react";

import { cn } from "@/lib/utils";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { BrandLogo } from "../brand-logo";
import { useSidebar } from "./sidebar-context";
import { SidebarNav } from "./sidebar-nav";
import { SidebarChatHistory } from "./sidebar-chat-history";
import { SidebarAccount } from "./sidebar-account";
import { SidebarResizer } from "./sidebar-resizer";

/// ロゴ（四つの点＋ワードマーク）。ホームへのリンクを兼ねる。
function Wordmark({ onNavigate, mobile }: { onNavigate?: () => void; mobile?: boolean }) {
  return (
    <Link
      href="/"
      onClick={onNavigate}
      className="rounded-md outline-none focus-visible:ring-2 focus-visible:ring-sidebar-ring"
      aria-label="ホームへ"
    >
      <BrandLogo size={mobile ? "touch" : "md"} />
    </Link>
  );
}

/// サイドバー内部レイアウト（デスクトップ aside / モバイルドロワで共用）。
/// `mobile` はスマホのドロワ: 折りたたみトグルの代わりに閉じる ×、ナビはタイル 1 行、履歴は大きい行。
export function SidebarContent({
  collapsed,
  onNavigate,
  mobile = false,
}: {
  collapsed: boolean;
  onNavigate?: () => void;
  mobile?: boolean;
}) {
  const { toggleCollapsed, setMobileOpen } = useSidebar();

  return (
    <div className="flex h-full flex-col bg-sidebar text-sidebar-foreground">
      {/* ヘッダ: ワードマーク＋折りたたみトグル（スマホは閉じる）。装飾アイコンは置かない */}
      {mobile ? (
        <div className="flex h-14 shrink-0 items-center pl-4 pr-2">
          <Wordmark onNavigate={onNavigate} mobile />
          <button
            type="button"
            onClick={() => setMobileOpen(false)}
            aria-label="メニューを閉じる"
            className="ml-auto flex size-11 items-center justify-center rounded-lg text-sidebar-foreground/60 outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring active:bg-sidebar-accent"
          >
            <X className="size-5" aria-hidden />
          </button>
        </div>
      ) : (
        <div
          className={cn(
            "flex h-14 shrink-0 items-center px-3",
            collapsed ? "justify-center" : "gap-1.5",
          )}
        >
          {!collapsed ? (
            <>
              <Wordmark onNavigate={onNavigate} />
              <button
                type="button"
                onClick={toggleCollapsed}
                aria-label="サイドバーを折りたたむ"
                aria-expanded
                className="ml-auto flex size-7 items-center justify-center rounded-md text-sidebar-foreground/55 outline-none transition-colors hover:bg-sidebar-accent hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring"
              >
                <PanelLeftClose className="size-[18px]" aria-hidden />
              </button>
            </>
          ) : (
            <Tooltip>
              <TooltipTrigger asChild>
                <button
                  type="button"
                  onClick={toggleCollapsed}
                  aria-label="サイドバーを開く"
                  aria-expanded={false}
                  className="flex size-9 items-center justify-center rounded-[9px] text-sidebar-foreground/55 outline-none transition-colors hover:bg-sidebar-accent hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring"
                >
                  <PanelLeftOpen className="size-[18px]" aria-hidden />
                </button>
              </TooltipTrigger>
              <TooltipContent side="right">サイドバーを開く</TooltipContent>
            </Tooltip>
          )}
        </div>
      )}

      {/* 上部固定ナビ（新しいチャット＋検索＋ドライブ） */}
      <div className="shrink-0">
        <SidebarNav collapsed={collapsed} mobile={mobile} onNavigate={onNavigate} />
      </div>

      {/* ナビと履歴の間を荒い破線で区切る（和のシグネチャ・rule-soft で控えめに）。 */}
      <div className={cn("shrink-0 pt-3", mobile ? "px-4" : "px-2.5")} aria-hidden>
        <div className="rule-soft shiki-dash-x h-1.5" />
      </div>

      {/* 中段: チャット履歴（見出し「履歴」と種類フィルタを含む） */}
      {collapsed ? <div className="h-1" /> : null}
      <SidebarChatHistory collapsed={collapsed} mobile={mobile} onNavigate={onNavigate} />

      {/* 最下部: アカウント（上端は荒い破線で区切る） */}
      <div className="shrink-0 shiki-dash-top">
        <SidebarAccount collapsed={collapsed} mobile={mobile} onNavigate={onNavigate} />
      </div>
    </div>
  );
}

/// デスクトップの aside。幅は context 由来、右端にリサイズハンドル。
/// 本文から少し浮かせた「フローティング パネル」にする（周囲 8px の間・角丸・淡い影）。
/// 角丸クリップは内側ラッパの overflow-hidden で行い、外側 aside は overflow を残して
/// リサイザ（右端外側に配置）が切れないようにする。
export function Sidebar() {
  const { collapsed, effectiveWidthPx } = useSidebar();
  const asideRef = React.useRef<HTMLElement | null>(null);

  return (
    <aside
      ref={asideRef}
      style={{ width: effectiveWidthPx }}
      data-collapsed={collapsed}
      className="relative m-2 hidden h-[calc(100dvh-1rem)] shrink-0 transition-[width] duration-200 ease-[var(--ease-standard)] md:block"
    >
      <div className="h-full overflow-hidden rounded-xl border border-sidebar-border bg-sidebar shadow-sm">
        <SidebarContent collapsed={collapsed} />
      </div>
      {!collapsed ? <SidebarResizer targetRef={asideRef} /> : null}
    </aside>
  );
}
