"use client";

import Link from "next/link";
import { useRouter, usePathname } from "next/navigation";
import { PenSquare, Search, type LucideIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import {
  APPS_NAV,
  DATA_NAV,
  DRIVE_ICON,
  DRIVE_ROOT,
  SKILLS_NAV,
  WORKFLOWS_NAV,
  isActivePath,
} from "@/lib/nav-config";
import { currentSeasonIndex, seasonVar } from "@/lib/season";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { NavItem } from "./nav-item";
import { SidebarDriveAccordion } from "./sidebar-drive-accordion";
import { useSidebar } from "./sidebar-context";

/// スマホのドロワのナビ 1 マス（アイコン上・ラベル下）。
function NavTile({
  icon: Icon,
  label,
  href,
  active,
  onNavigate,
}: {
  icon: LucideIcon;
  label: string;
  href: string;
  active: boolean;
  onNavigate?: () => void;
}) {
  return (
    <Link
      href={href}
      onClick={onNavigate}
      aria-current={active ? "page" : undefined}
      className={cn(
        "flex h-[62px] min-w-0 flex-col items-center justify-center gap-1 rounded-xl px-0.5 text-[11px] tracking-[-0.02em] outline-none transition-colors",
        "focus-visible:ring-2 focus-visible:ring-sidebar-ring",
        active
          ? "bg-sidebar-accent font-medium text-sidebar-foreground"
          : "text-sidebar-foreground/70 active:bg-sidebar-accent/60",
      )}
    >
      <Icon
        className={cn("size-5 shrink-0", active ? "text-sidebar-foreground" : "text-sidebar-foreground/60")}
        aria-hidden
      />
      <span className="max-w-full truncate">{label}</span>
    </Link>
  );
}

/// スマホのドロワ上部: 「新しいチャット」（主ボタン）と検索を 1 段に、その下にナビ 5 マスを 1 行。
/// 履歴に使える縦幅を広げるため、ドライブの子メニューはここに出さない（ドライブ画面側の ⋯ が導線）。
function MobileNav({ onNavigate }: { onNavigate?: () => void }) {
  const router = useRouter();
  const pathname = usePathname();
  const { setSearchOpen } = useSidebar();
  const tiles = [
    { key: "drive", label: "ドライブ", href: DRIVE_ROOT, icon: DRIVE_ICON },
    SKILLS_NAV,
    APPS_NAV,
    WORKFLOWS_NAV,
    DATA_NAV,
  ];

  return (
    <div className="flex flex-col gap-3 px-3">
      <div className="flex gap-2">
        <button
          type="button"
          onClick={() => {
            router.push("/");
            onNavigate?.();
          }}
          className="flex h-12 min-w-0 flex-1 items-center justify-center gap-2 rounded-xl bg-primary px-4 text-[15px] font-medium text-primary-foreground outline-none transition-[transform,background-color] duration-[var(--duration-fast)] ease-[var(--ease-standard)] focus-visible:ring-2 focus-visible:ring-sidebar-ring focus-visible:ring-offset-2 focus-visible:ring-offset-sidebar active:scale-[0.98] active:bg-primary/90"
        >
          <PenSquare className="size-[18px] shrink-0" aria-hidden />
          新しいチャット
        </button>
        {/* 検索パレットはドロワの上に重なる。結果を選ぶとドロワも閉じる（sidebar-search の go）。 */}
        <button
          type="button"
          onClick={() => setSearchOpen(true)}
          aria-label="検索"
          className="flex size-12 shrink-0 items-center justify-center rounded-xl border border-sidebar-border bg-sidebar-accent/40 text-sidebar-foreground/70 outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring active:bg-sidebar-accent"
        >
          <Search className="size-5" aria-hidden />
        </button>
      </div>
      <nav aria-label="メインナビゲーション" className="grid grid-cols-5 gap-1">
        {tiles.map((t) => (
          <NavTile
            key={t.key}
            icon={t.icon}
            label={t.label}
            href={t.href}
            active={isActivePath(t.href, pathname)}
            onNavigate={onNavigate}
          />
        ))}
      </nav>
    </div>
  );
}

/// サイドバー上部: 新規チャット（主役 CTA）＋ 検索（検索ボックス風）＋ ナビ（ドライブ/スキル/アプリ/ワークフロー）。
/// 検索ダイアログ本体はシェルで単一管理し、ここは開くトリガーのみ（多重マウント回避）。
/// `mobile` はスマホのドロワ用の詰めたレイアウト（`MobileNav`）。
export function SidebarNav({
  collapsed,
  mobile = false,
  onNavigate,
}: {
  collapsed: boolean;
  mobile?: boolean;
  onNavigate?: () => void;
}) {
  const router = useRouter();
  const pathname = usePathname();
  const { setSearchOpen } = useSidebar();

  if (mobile) return <MobileNav onNavigate={onNavigate} />;

  const startNewChat = () => {
    router.push("/");
    onNavigate?.();
  };

  return (
    <div className="flex flex-col gap-2 px-2.5">
      {/* 主役 CTA: 新しいチャット（白の浮き上がるボタン＋季節色のペン・押下フィードバック）。
          重い navy 塗りは避け、カード面＋境界＋淡い影で「押せる主役」を軽やかに示す。 */}
      {collapsed ? (
        <Tooltip>
          <TooltipTrigger asChild>
            <button
              type="button"
              onClick={startNewChat}
              aria-label="新しいチャット"
              className="mx-auto flex size-9 items-center justify-center rounded-[10px] bg-sidebar-accent text-sidebar-foreground outline-none transition-[transform,background-color] duration-[var(--duration-fast)] ease-[var(--ease-standard)] hover:bg-sidebar-accent/70 focus-visible:ring-2 focus-visible:ring-sidebar-ring active:scale-95"
            >
              <PenSquare
                className="size-[18px]"
                style={{ color: seasonVar(currentSeasonIndex()) }}
                aria-hidden
              />
            </button>
          </TooltipTrigger>
          <TooltipContent side="right">新しいチャット</TooltipContent>
        </Tooltip>
      ) : (
        <button
          type="button"
          onClick={startNewChat}
          className="flex h-9 w-full items-center gap-2 rounded-[10px] bg-sidebar-accent px-3 text-[13.5px] font-medium text-sidebar-foreground outline-none transition-[transform,background-color] duration-[var(--duration-fast)] ease-[var(--ease-standard)] hover:bg-sidebar-accent/70 focus-visible:ring-2 focus-visible:ring-sidebar-ring active:scale-[0.98]"
        >
          <PenSquare
            className="size-[18px] shrink-0"
            style={{ color: seasonVar(currentSeasonIndex()) }}
            aria-hidden
          />
          新しいチャット
        </button>
      )}

      {/* 検索: 検索ボックス風のトリガ（実体はシェルのパレット）。 */}
      {collapsed ? (
        <Tooltip>
          <TooltipTrigger asChild>
            <button
              type="button"
              onClick={() => setSearchOpen(true)}
              aria-label="検索"
              className="mx-auto flex size-9 items-center justify-center rounded-[10px] text-sidebar-foreground/60 outline-none transition-colors hover:bg-sidebar-accent hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring"
            >
              <Search className="size-[18px]" aria-hidden />
            </button>
          </TooltipTrigger>
          <TooltipContent side="right">検索</TooltipContent>
        </Tooltip>
      ) : (
        <button
          type="button"
          onClick={() => setSearchOpen(true)}
          className="group flex h-9 w-full items-center gap-2.5 rounded-[10px] border border-sidebar-border bg-sidebar-accent/40 px-2.5 text-left outline-none transition-colors hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-sidebar-ring"
        >
          <Search className="size-4 shrink-0 text-sidebar-foreground/45" aria-hidden />
          <span className="flex-1 text-[13px] text-sidebar-foreground/45">検索</span>
          {/* ショートカット表記はキーボードのある（マウス操作の）端末だけ。 */}
          <kbd className="hidden rounded border border-sidebar-border bg-sidebar px-1.5 py-0.5 text-[10px] font-medium leading-none text-sidebar-foreground/45 pointer-fine:inline-block">
            ⌘K
          </kbd>
        </button>
      )}

      {/* ナビ（検索/新規チャットとは間を空けて視覚分離）。 */}
      <nav
        aria-label="メインナビゲーション"
        className={cn("flex flex-col gap-0.5", collapsed ? "pt-1" : "pt-1.5")}
      >
        <SidebarDriveAccordion collapsed={collapsed} onNavigate={onNavigate} />
        <NavItem
          icon={SKILLS_NAV.icon}
          label={SKILLS_NAV.label}
          collapsed={collapsed}
          active={isActivePath(SKILLS_NAV.href, pathname)}
          onClick={() => {
            router.push(SKILLS_NAV.href);
            onNavigate?.();
          }}
        />
        <NavItem
          icon={APPS_NAV.icon}
          label={APPS_NAV.label}
          collapsed={collapsed}
          active={isActivePath(APPS_NAV.href, pathname)}
          onClick={() => {
            router.push(APPS_NAV.href);
            onNavigate?.();
          }}
        />
        <NavItem
          icon={WORKFLOWS_NAV.icon}
          label={WORKFLOWS_NAV.label}
          collapsed={collapsed}
          active={isActivePath(WORKFLOWS_NAV.href, pathname)}
          onClick={() => {
            router.push(WORKFLOWS_NAV.href);
            onNavigate?.();
          }}
        />
        <NavItem
          icon={DATA_NAV.icon}
          label={DATA_NAV.label}
          collapsed={collapsed}
          active={isActivePath(DATA_NAV.href, pathname)}
          onClick={() => {
            router.push(DATA_NAV.href);
            onNavigate?.();
          }}
        />
      </nav>
    </div>
  );
}
