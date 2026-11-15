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
  MESSAGES_NAV,
  SKILLS_NAV,
  WORKFLOWS_NAV,
  isActivePath,
} from "@/lib/nav-config";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { useUnreadMentions } from "@/hooks/use-unread-mentions";
import { NavItem } from "./nav-item";
import { useSidebar } from "./sidebar-context";

/// スマホのドロワのナビ 1 マス（アイコン上・ラベル下）。
/// 未読のメンションの件数（ナビのメッセージ項目の右上）。0 なら出さない。
export function MentionBadge({ count, className }: { count: number; className?: string }) {
  if (count <= 0) return null;
  return (
    <span
      className={cn(
        "flex h-4 min-w-4 items-center justify-center rounded-full bg-primary px-1 text-[10px] font-semibold leading-none text-primary-foreground ring-2 ring-sidebar",
        className,
      )}
      data-testid="messages-nav-badge"
    >
      <span className="sr-only">未読のメンション </span>
      {count > 99 ? "99+" : count}
    </span>
  );
}

function NavTile({
  icon: Icon,
  label,
  href,
  active,
  badge = 0,
  onNavigate,
}: {
  icon: LucideIcon;
  label: string;
  href: string;
  active: boolean;
  badge?: number;
  onNavigate?: () => void;
}) {
  return (
    <Link
      href={href}
      onClick={onNavigate}
      aria-current={active ? "page" : undefined}
      className={cn(
        "relative flex h-[52px] min-w-0 flex-col items-center justify-center gap-0.5 rounded-xl px-0.5 text-[11px] tracking-[-0.02em] outline-none transition-colors",
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
      <MentionBadge count={badge} className="absolute right-1 top-1.5" />
    </Link>
  );
}

/// スマホのドロワ上部: 「新しいチャット」（主ボタン）と検索を 1 段に、その下にナビ 6 マスを 3×2 で並べる
/// （1 行に 6 つ並べると「メッセージ」「ワークフロー」のラベルが切れるため。マスを低くして縦幅を抑える）。
/// 履歴に使える縦幅を広げるため、ドライブの子メニューはここに出さない（区画の切替はドライブ画面上端のタブ）。
function MobileNav({ onNavigate }: { onNavigate?: () => void }) {
  const router = useRouter();
  const pathname = usePathname();
  const { setSearchOpen } = useSidebar();
  const mentions = useUnreadMentions();
  const tiles = [
    { key: "drive", label: "ドライブ", href: DRIVE_ROOT, icon: DRIVE_ICON },
    MESSAGES_NAV,
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
      <nav aria-label="メインナビゲーション" className="grid grid-cols-3 gap-1">
        {tiles.map((t) => (
          <NavTile
            key={t.key}
            icon={t.icon}
            label={t.label}
            href={t.href}
            active={isActivePath(t.href, pathname)}
            badge={t.key === MESSAGES_NAV.key ? mentions : 0}
            onNavigate={onNavigate}
          />
        ))}
      </nav>
    </div>
  );
}

/// PC のメインナビ（ドライブ・メッセージ・スキル・アプリ・ワークフロー・データ）。タイルの色は機能ごとに固定し、
/// 四季トークン（data は primary）を当てる。地色の混色は oklab（oklch だと白の色相 0° に引かれて色がずれる）。色だけに頼らず、選択中は枠・地色・太字でも示す。
const MAIN_NAV: { key: string; label: string; href: string; icon: LucideIcon; tone: string }[] = [
  { key: "drive", label: "ドライブ", href: DRIVE_ROOT, icon: DRIVE_ICON, tone: "var(--season-winter)" },
  { ...MESSAGES_NAV, tone: "var(--season-autumn)" },
  { ...SKILLS_NAV, tone: "var(--season-spring)" },
  { ...APPS_NAV, tone: "var(--season-autumn)" },
  { ...WORKFLOWS_NAV, tone: "var(--season-summer)" },
  { ...DATA_NAV, tone: "var(--primary)" },
];

/// PC のナビ 1 マス（アイコン上・ラベル下）。`--tile` に機能の色を受け取る。
const TILE_CLASS = cn(
  "flex h-[60px] min-w-0 flex-col items-center justify-center gap-1 rounded-[12px] px-1 text-[11.5px] tracking-[-0.02em] outline-none",
  "bg-card text-sidebar-foreground/75 ring-1 ring-sidebar-border",
  "transition-[background-color,box-shadow,color,transform] duration-[var(--duration-fast)] ease-[var(--ease-standard)]",
  "hover:bg-[color-mix(in_oklab,var(--tile)_8%,var(--card))] hover:text-sidebar-foreground",
  "focus-visible:ring-2 focus-visible:ring-sidebar-ring active:scale-[0.97] pointer-coarse:h-16",
);
const TILE_ACTIVE_CLASS =
  "bg-[color-mix(in_oklab,var(--tile)_14%,var(--card))] font-semibold text-sidebar-foreground ring-[1.5px] ring-[var(--tile)] hover:bg-[color-mix(in_oklab,var(--tile)_14%,var(--card))]";

function TileInner({ icon: Icon, label }: { icon: LucideIcon; label: string }) {
  return (
    <>
      <Icon className="size-5 shrink-0 text-[var(--tile)]" aria-hidden />
      <span className="max-w-full truncate">{label}</span>
    </>
  );
}

/// PC の展開時: 主ボタン「新しいチャット」と検索（スマホのドロワと同じ 1 段）、その下にナビ 6 項目の 3×2 タイル
/// （アイコン上・ラベル下）。ドライブの区画はサイドバーに出さず、ドライブ画面上端のタブが担う。
function DesktopNav({ onNavigate }: { onNavigate?: () => void }) {
  const router = useRouter();
  const pathname = usePathname();
  const { setSearchOpen } = useSidebar();
  const mentions = useUnreadMentions();

  return (
    <div className="flex flex-col gap-3 px-2.5">
      <div className="flex gap-1.5">
        <button
          type="button"
          onClick={() => {
            router.push("/");
            onNavigate?.();
          }}
          className="flex h-10 min-w-0 flex-1 items-center justify-center gap-2 rounded-[12px] bg-primary px-3 text-[13.5px] font-semibold text-primary-foreground shadow-sm outline-none transition-[transform,background-color] duration-[var(--duration-fast)] ease-[var(--ease-standard)] hover:bg-primary/90 focus-visible:ring-2 focus-visible:ring-sidebar-ring focus-visible:ring-offset-2 focus-visible:ring-offset-sidebar active:scale-[0.98]"
        >
          <PenSquare className="size-[18px] shrink-0" aria-hidden />
          新しいチャット
        </button>
        {/* 検索（実体はシェルのパレット）。⌘K はツールチップで示す。 */}
        <Tooltip>
          <TooltipTrigger asChild>
            <button
              type="button"
              onClick={() => setSearchOpen(true)}
              aria-label="文書・チャットを検索"
              aria-keyshortcuts="Meta+K Control+K"
              className="flex size-10 shrink-0 items-center justify-center rounded-[12px] bg-card text-sidebar-foreground/70 ring-1 ring-sidebar-border outline-none transition-colors hover:bg-sidebar-accent hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring active:scale-[0.97]"
            >
              <Search className="size-[18px]" aria-hidden />
            </button>
          </TooltipTrigger>
          <TooltipContent side="bottom">検索（⌘K）</TooltipContent>
        </Tooltip>
      </div>

      <nav aria-label="メインナビゲーション" className="grid grid-cols-3 gap-1.5">
        {MAIN_NAV.map((item) => {
          const active = isActivePath(item.href, pathname);
          return (
            <Link
              key={item.key}
              href={item.href}
              onClick={onNavigate}
              // 配下（/drive/trash 等）では現在地の "page" を区画タブに譲り、ここは "true"（含む）にする。
              aria-current={active ? (pathname === item.href ? "page" : "true") : undefined}
              className={cn(TILE_CLASS, "relative", active && TILE_ACTIVE_CLASS)}
              style={{ ["--tile" as string]: item.tone }}
            >
              <TileInner icon={item.icon} label={item.label} />
              {item.key === MESSAGES_NAV.key ? <MentionBadge count={mentions} className="absolute right-1.5 top-1.5" /> : null}
            </Link>
          );
        })}
      </nav>
    </div>
  );
}

/// サイドバー上部のナビ。展開時はタイル（`DesktopNav`）、レール（collapsed）時はアイコンのみの縦列、
/// スマホのドロワは詰めたレイアウト（`MobileNav`）。
/// 検索ダイアログ本体はシェルで単一管理し、ここは開くトリガーのみ（多重マウント回避）。
export function SidebarNav({
  collapsed,
  mobile = false,
  onNavigate,
}: {
  collapsed: boolean;
  mobile?: boolean;
  onNavigate?: () => void;
}) {
  if (mobile) return <MobileNav onNavigate={onNavigate} />;
  if (!collapsed) return <DesktopNav onNavigate={onNavigate} />;
  return <RailNav onNavigate={onNavigate} />;
}

/// レール（collapsed）: アイコンのみの縦列。ラベルはツールチップで補う。
function RailNav({ onNavigate }: { onNavigate?: () => void }) {
  const router = useRouter();
  const pathname = usePathname();
  const { setSearchOpen } = useSidebar();
  const mentions = useUnreadMentions();

  return (
    <div className="flex flex-col gap-2 px-2.5">
      <Tooltip>
        <TooltipTrigger asChild>
          <button
            type="button"
            onClick={() => {
              router.push("/");
              onNavigate?.();
            }}
            aria-label="新しいチャット"
            className="mx-auto flex size-9 items-center justify-center rounded-[10px] bg-primary text-primary-foreground outline-none transition-[transform,background-color] duration-[var(--duration-fast)] ease-[var(--ease-standard)] hover:bg-primary/90 focus-visible:ring-2 focus-visible:ring-sidebar-ring active:scale-95"
          >
            <PenSquare className="size-[18px]" aria-hidden />
          </button>
        </TooltipTrigger>
        <TooltipContent side="right">新しいチャット</TooltipContent>
      </Tooltip>
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
      <nav aria-label="メインナビゲーション" className="flex flex-col gap-0.5 pt-1">
        {MAIN_NAV.map((item) => (
          <div key={item.key} className="relative w-9">
            <NavItem
              icon={item.icon}
              label={item.label}
              href={item.href}
              collapsed
              active={isActivePath(item.href, pathname)}
              onClick={onNavigate}
            />
            {item.key === MESSAGES_NAV.key ? (
              <MentionBadge count={mentions} className="pointer-events-none absolute -right-1 -top-1" />
            ) : null}
          </div>
        ))}
      </nav>
    </div>
  );
}
