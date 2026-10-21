"use client";

/// 入力欄の本文の先頭に入る語（選んだツール・/ コマンド・#768）。
///
/// ChatGPT の入力欄のように、選んだものを本文の一部として見せる。textarea の 1 行目の先頭に
/// 重ねて置き、textarea には語の幅だけ `text-indent` を付ける。続けて打った文字は語の右から始まり、
/// 折り返した 2 行目以降は左端から始まる。語を押すと外れる（本文の先頭で Backspace も同じ・
/// Composer 側）。
///
/// 位置は textarea の 1 行目に合わせる（呼び出し側が textarea の余白と同じ top/left を渡す）。

import * as React from "react";
import type { LucideIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { usePromptInput } from "@/components/prompt-kit/prompt-input";

export type ComposerToken = {
  key: string;
  label: string;
  icon: LucideIcon;
  /// 押したときの説明（読み上げ・ツールチップ）。例:「Web 検索を外す」。
  removeLabel: string;
  onRemove: () => void;
  testId?: string;
};

/// 語と本文の間の空き。
export const TOKEN_GAP = 6;

export function ComposerTokens({
  tokens,
  onWidthChange,
  scrollTop = 0,
  className,
}: {
  tokens: ComposerToken[];
  /// 並びの幅（px）。textarea の `text-indent` に使う。
  onWidthChange: (width: number) => void;
  /// textarea のスクロール量。長文でスクロールしたら、語も 1 行目と一緒に上へ流す。
  scrollTop?: number;
  className?: string;
}) {
  const ref = React.useRef<HTMLSpanElement | null>(null);
  const { textareaRef } = usePromptInput();
  // 語の並びが変わったときだけ測り直す（毎回の入力で ResizeObserver を作り直さない）。
  const signature = tokens.map((t) => `${t.key}:${t.label}`).join("|");
  React.useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const report = () => onWidthChange(Math.ceil(el.getBoundingClientRect().width));
    report();
    if (typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(report);
    ro.observe(el);
    return () => ro.disconnect();
  }, [onWidthChange, signature]);

  return (
    <span
      ref={ref}
      style={scrollTop ? { transform: `translateY(${-scrollTop}px)` } : undefined}
      className={cn(
        "absolute z-[1] inline-flex max-w-[calc(100%-4rem)] items-center gap-1 overflow-hidden whitespace-nowrap",
        className,
      )}
    >
      {tokens.map((t) => (
        <button
          key={t.key}
          type="button"
          onClick={(e) => {
            // 入力欄の枠のクリック（textarea へフォーカス）より先に外す。
            e.stopPropagation();
            t.onRemove();
            // 押したボタンは消えるので、続けて打てるよう入力欄へ移す（キーボード操作、またはマウス）。
            if (e.detail === 0 || window.matchMedia("(pointer: fine)").matches) {
              textareaRef.current?.focus({ preventScroll: true });
            }
          }}
          aria-label={t.removeLabel}
          title={t.removeLabel}
          data-testid={t.testId}
          className={cn(
            // 並びが長いときは名前を省略して縮める（語ごと見えなくならないように）。
            // 色は四季の冬（雪空）を淡く敷き、文字は前景色と混ぜて読める濃さにする（Shiki のバッジと同じ作り）。
            "inline-flex min-w-0 max-w-[11rem] shrink items-center gap-1 rounded-md px-1.5 font-medium transition-colors",
            "bg-season-winter/18 text-[color-mix(in_oklab,var(--season-winter)_82%,var(--foreground))] hover:bg-season-winter/30",
            "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring",
          )}
        >
          <t.icon className="size-[1.05em] shrink-0 text-season-winter" aria-hidden />
          <span className="truncate">{t.label}</span>
        </button>
      ))}
    </span>
  );
}
