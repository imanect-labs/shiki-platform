"use client";

import { usePathname } from "next/navigation";

import { currentSeasonIndex, seasonVar } from "@/lib/season";
import { cn } from "@/lib/utils";
import { resolvePageIcon, resolvePageTitle } from "@/lib/nav-config";
import { useSidebar } from "./sidebar/sidebar-context";
import {
  ChromeActions,
  ChromeBackButton,
  ChromeBar,
  ChromeDivider,
  usePageChromeValue,
} from "./page-header-context";

/// PC（md 以上）の上部バー。ページが `usePageChrome` で登録した spec を描き、
/// 無ければルート由来の現在地（アイコン＋タイトル）を既定表示する。
/// 下端は破線ルール（shiki-dash-bottom）で本文とやわらかく分ける。
/// スマホではタイトルバーを出さない（`MobileChrome` が角に操作を浮かせる）。
export function Header() {
  const pathname = usePathname();
  const spec = usePageChromeValue();
  const PageIcon = resolvePageIcon(pathname);
  const title = resolvePageTitle(pathname);

  return (
    <header className="shiki-dash-bottom hidden h-14 shrink-0 items-center gap-2.5 bg-background/80 px-6 backdrop-blur supports-[backdrop-filter]:bg-background/60 md:flex">
      {spec ? (
        <ChromeBar spec={spec} />
      ) : (
        <div className="flex min-w-0 flex-1 items-center gap-2">
          <span
            className="flex size-8 items-center justify-center text-foreground/70"
            role="img"
            aria-label={title}
            title={title}
          >
            <PageIcon className="size-[19px]" strokeWidth={2} aria-hidden />
          </span>
          <span className="truncate text-sm font-medium text-foreground/80">{title}</span>
        </div>
      )}
    </header>
  );
}

/// スマホの浮いた操作群の「台」。PC の浮いたサイドバー（角丸・沈んだ面・淡い影）と同じ素材で、
/// 角は丸めすぎない（丸いピルにしない）。
export const CHROME_TRAY = cn(
  "pointer-events-auto flex h-12 items-center rounded-[14px] border border-sidebar-border px-0.5",
  "bg-sidebar/95 shadow-[var(--elevation-md)] backdrop-blur supports-[backdrop-filter]:bg-sidebar/80",
);

/// 一筋の季（いちすじのき）。三本線のメニューを Shiki の線で描く: 上の一本だけを今の季節の色の
/// 短い実線にし、下の二本は区切り線と同じ細い破線で控えめにする。季節色は描画時の月から決める
/// （empty-state・editor-loading と同じ）。サーバとクライアントで月を跨いだ瞬間だけ stroke が
/// ずれ得るため、その差分の警告は抑える（見た目は次の描画で揃う）。
export function MenuMark({ className }: { className?: string }) {
  return (
    <svg
      aria-hidden
      viewBox="0 0 24 24"
      fill="none"
      strokeLinecap="round"
      className={cn("size-6", className)}
    >
      <path
        d="M4.5 7h8.5"
        stroke={seasonVar(currentSeasonIndex())}
        strokeWidth={2.6}
        suppressHydrationWarning
        className="origin-left [transform-box:fill-box] transition-transform duration-[var(--duration-normal)] ease-[var(--ease-standard)] group-hover:scale-x-[1.18] group-active:scale-x-[1.18]"
      />
      <path d="M4.5 12h15" stroke="currentColor" strokeOpacity={0.7} strokeWidth={1.6} strokeDasharray="3.4 2.2" />
      <path d="M4.5 17h15" stroke="currentColor" strokeOpacity={0.7} strokeWidth={1.6} strokeDasharray="3.4 2.2" />
    </svg>
  );
}

/// ドロワ（ナビゲーション）を開くボタン。顔は一筋の季（`MenuMark`）。
export function MenuMarkButton({
  onClick,
  label = "メニューを開く",
}: {
  onClick: () => void;
  label?: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      title={label}
      className="group flex size-11 items-center justify-center rounded-[10px] text-foreground outline-none transition-colors hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-ring active:scale-[0.94]"
    >
      <MenuMark />
    </button>
  );
}

/// スマホ（md 未満）の浮いた操作群。左上にメニュー（＋戻る）、右上にページの操作を置く。
/// タイトルバーの代わりなので、どの画面でもドロワへの導線が必ず残る（没入エディタ・
/// 読み込み中・notfound でも消えない）。
export function MobileChrome() {
  const { setMobileOpen } = useSidebar();
  const spec = usePageChromeValue();
  const hasRight = Boolean(
    spec && (spec.primary || spec.actions || spec.assistant || (spec.menu && spec.menu.length > 0)),
  );

  return (
    <div className="pointer-events-none absolute inset-x-0 top-0 z-30 md:hidden">
      {/* スクロールした本文がボタンの間に透けて見えないよう、上端を背景色でぼかす。本文を
          ボタンの下まで流すページは深め、それ以外は本文の開始位置（4rem）で消える。 */}
      <div
        aria-hidden
        className={cn(
          "absolute inset-x-0 top-0 bg-gradient-to-b from-background from-30% to-transparent",
          spec?.flowUnder
            ? "h-[calc(5.5rem+env(safe-area-inset-top))]"
            : "h-[calc(4rem+env(safe-area-inset-top))]",
        )}
      />

      <div className="relative flex items-start justify-between gap-2 px-2.5 pt-[max(0.5rem,env(safe-area-inset-top))]">
        <div className={CHROME_TRAY}>
          <MenuMarkButton onClick={() => setMobileOpen(true)} />
          {spec?.back ? (
            <>
              <ChromeDivider />
              <ChromeBackButton back={spec.back} />
            </>
          ) : null}
        </div>
        {hasRight && spec ? (
          <div className={cn(CHROME_TRAY, spec.primary && "pl-1")}>
            <ChromeActions spec={spec} divided />
          </div>
        ) : null}
      </div>
    </div>
  );
}
