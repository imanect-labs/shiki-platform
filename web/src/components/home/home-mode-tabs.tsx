"use client";

import { cn } from "@/lib/utils";
import { HOME_MODES, type HomeMode } from "@/lib/home-modes";
import { useRovingSelect } from "./use-roving-select";

/// モードの設定の段（tabpanel）の id。タブの aria-controls から指す。
export const HOME_MODE_PANEL_ID = "home-mode-panel";

export function homeModeTabId(mode: HomeMode): string {
  return `home-mode-tab-${mode}`;
}

const KEYS = HOME_MODES.map((m) => m.key);

/// ホームの入力欄の上に置くモード切替（チャット／社内検索／エージェント／作成・#964）。
/// 形はサイドバーの履歴の切替と同じ丸いピル。←→・Home・End で移れる（tablist の作法）。
export function HomeModeTabs({
  value,
  onChange,
}: {
  value: HomeMode;
  onChange: (mode: HomeMode) => void;
}) {
  const roving = useRovingSelect(KEYS, value, onChange);

  return (
    <div
      role="tablist"
      aria-label="モード"
      className="inline-flex max-w-full gap-0.5 rounded-full border border-border bg-muted p-[3px]"
    >
      {HOME_MODES.map((m, i) => {
        const active = m.key === value;
        const Icon = m.icon;
        return (
          <button
            key={m.key}
            ref={roving.ref(i)}
            id={homeModeTabId(m.key)}
            type="button"
            role="tab"
            aria-selected={active}
            aria-controls={HOME_MODE_PANEL_ID}
            tabIndex={roving.tabIndex(i)}
            data-testid={`home-mode-${m.key}`}
            onClick={() => onChange(m.key)}
            onKeyDown={roving.onKeyDown(i)}
            className={cn(
              "inline-flex h-8 items-center gap-1.5 whitespace-nowrap rounded-full px-3 text-[13px] transition-colors sm:px-3.5",
              "outline-none focus-visible:ring-2 focus-visible:ring-ring",
              active
                ? "bg-card text-foreground shadow-xs ring-1 ring-border"
                : "text-foreground/65 hover:text-foreground",
            )}
          >
            <Icon className="hidden size-[15px] sm:block" aria-hidden />
            {m.label}
          </button>
        );
      })}
    </div>
  );
}
