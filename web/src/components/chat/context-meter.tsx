"use client";

import * as React from "react";

import { cn } from "@/lib/utils";
import type { ContextCategory, ContextUsage } from "@/lib/chat-api";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";

/// カテゴリの表示名と色（四季トークン＋プライマリ・ライト/ダークで自動反転）。
const CATEGORY: Record<ContextCategory, { label: string; hint: string; color: string }> = {
  system: { label: "システムプロンプト", hint: "指示・スキル・開いている文書", color: "var(--primary)" },
  tools: { label: "ツール定義", hint: "読み込まれているツール", color: "var(--season-winter)" },
  messages: { label: "会話", hint: "発話・回答・思考", color: "var(--season-summer)" },
  documents: { label: "資料", hint: "検索・ファイル・Web の内容", color: "var(--season-autumn)" },
  tool_results: { label: "ツール結果", hint: "そのほかのツールの出力", color: "var(--season-spring)" },
};

/// 応答用に空けておく分（斜線）。
const RESERVED_BG =
  "repeating-linear-gradient(135deg, color-mix(in oklab, var(--muted-foreground) 55%, transparent) 0 2px, transparent 2px 5px)";

/// トークン数を短く（1,234 → 1.2k、1,234,567 → 1.2M）。
export function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${trimZero((n / 1000).toFixed(n < 10_000 ? 1 : 0))}k`;
  return `${trimZero((n / 1_000_000).toFixed(1))}M`;
}

function trimZero(s: string): string {
  return s.replace(/\.0$/, "");
}

/// 割合（0〜100）を表示用に。0 より大きく 1% 未満は「<1%」。
export function formatPercent(p: number): string {
  if (p > 0 && p < 1) return "<1%";
  return `${Math.round(p)}%`;
}

/// 使用率に応じた色（余裕あり→注意→逼迫）。
function levelColor(percent: number | null): string {
  if (percent === null || percent < 70) return "var(--primary)";
  if (percent < 90) return "var(--season-autumn)";
  return "var(--destructive)";
}

/// 使用率のリング（窓が分からなければ枠だけ）。
function Ring({ percent }: { percent: number | null }) {
  const r = 7;
  const c = 2 * Math.PI * r;
  const filled = percent === null ? 0 : Math.min(Math.max(percent, 0), 100) / 100;
  return (
    <svg viewBox="0 0 18 18" className="size-[18px] shrink-0 -rotate-90" aria-hidden>
      <circle
        cx="9"
        cy="9"
        r={r}
        fill="none"
        strokeWidth="2.5"
        style={{ stroke: "color-mix(in oklab, var(--muted-foreground) 25%, transparent)" }}
      />
      {filled > 0 ? (
        <circle
          cx="9"
          cy="9"
          r={r}
          fill="none"
          strokeWidth="2.5"
          strokeLinecap="round"
          // 1% 未満でも点として見えるよう最小の弧を残す。
          strokeDasharray={`${Math.max(filled * c, 1.5)} ${c}`}
          style={{
            stroke: levelColor(percent),
            transition: "stroke-dasharray var(--duration-normal) var(--ease-standard)",
          }}
        />
      ) : null}
    </svg>
  );
}

/// 横棒の 1 区間。
function Segment({ share, background }: { share: number; background: string }) {
  if (share <= 0) return null;
  return (
    <div
      className="h-full border-r-2 border-popover last:border-r-0"
      style={{ width: `${share * 100}%`, minWidth: 4, background }}
    />
  );
}

/// コンテキストの使用量（何がどれだけ占めているか）。入力欄の操作段に置くリング＋ポップオーバー。
///
/// 値は**直前の LLM リクエスト**のもの（生成中はステップごとに更新される）。総量はモデルの実測、
/// 内訳は文字数からの見積りの比で按分したもの（サーバ側 `agent_core::context_usage`）。
export function ContextMeter({ usage, className }: { usage: ContextUsage; className?: string }) {
  const used = usage.used_tokens;
  const window = usage.window_tokens ?? null;
  const reserved = window ? Math.min(usage.reserved_output_tokens ?? 0, window) : 0;
  const free = window ? Math.max(window - used - reserved, 0) : 0;
  const percent = window ? (used / window) * 100 : null;
  const label =
    percent === null
      ? `コンテキスト: ${formatTokens(used)} トークン使用`
      : `コンテキスト: ${formatPercent(percent)} 使用中`;
  const cats = usage.categories.map((c) => ({ ...c, ...CATEGORY[c.category] }));

  return (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          aria-label={label}
          title={label}
          className={cn(
            "inline-flex h-9 shrink-0 items-center gap-1.5 rounded-full px-2 text-[12px] font-medium tabular-nums text-muted-foreground transition-colors",
            "hover:bg-secondary hover:text-foreground data-[state=open]:bg-secondary data-[state=open]:text-foreground",
            "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-card",
            className,
          )}
        >
          <Ring percent={percent} />
          <span>{percent === null ? formatTokens(used) : formatPercent(percent)}</span>
        </button>
      </PopoverTrigger>
      <PopoverContent align="end" side="top" className="w-80 p-0">
        <div className="px-4 pt-3.5">
          <div className="flex items-baseline justify-between gap-3">
            <h3 className="text-sm font-semibold">コンテキスト</h3>
            <p className="text-xs tabular-nums text-muted-foreground">
              <span className="font-medium text-foreground">{formatTokens(used)}</span>
              {window ? ` / ${formatTokens(window)}` : null} トークン
              {percent !== null ? (
                <span className="ml-1.5 font-semibold" style={{ color: levelColor(percent) }}>
                  {formatPercent(percent)}
                </span>
              ) : null}
            </p>
          </div>

          {/* 窓全体: 使用中（内訳の色）・応答用の予約（斜線）・空き。 */}
          {window ? (
            <>
              <div
                className="mt-2.5 flex h-2 overflow-hidden rounded-full bg-muted"
                role="img"
                aria-label={`使用中 ${formatPercent(percent ?? 0)}、応答用の予約 ${formatPercent((reserved / window) * 100)}、空き ${formatPercent((free / window) * 100)}`}
              >
                {cats.map((c) => (
                  <Segment key={c.category} share={c.tokens / window} background={c.color} />
                ))}
                <Segment share={reserved / window} background={RESERVED_BG} />
              </div>
              <dl className="mt-2 flex gap-4 text-[11px] text-muted-foreground">
                <div className="flex items-center gap-1.5">
                  <span className="size-2 rounded-[2px]" style={{ background: RESERVED_BG }} aria-hidden />
                  <dt>応答用の予約</dt>
                  <dd className="tabular-nums text-foreground">{formatTokens(reserved)}</dd>
                </div>
                <div className="flex items-center gap-1.5">
                  <span className="size-2 rounded-[2px] border border-border bg-muted" aria-hidden />
                  <dt>空き</dt>
                  <dd className="tabular-nums text-foreground">
                    {formatTokens(free)}（{formatPercent((free / window) * 100)}）
                  </dd>
                </div>
              </dl>
            </>
          ) : null}
        </div>

        {/* 使用中の内訳（使用中を 100% とした割合）。 */}
        <div className="mt-3 border-t px-4 pt-3">
          <p className="text-[11px] font-medium text-muted-foreground">使用中の内訳</p>
          <div
            className="mt-2 flex h-2.5 overflow-hidden rounded-full bg-muted"
            role="img"
            aria-label={cats
              .map((c) => `${c.label} ${formatPercent((c.tokens / Math.max(used, 1)) * 100)}`)
              .join("、")}
          >
            {cats.map((c) => (
              <Segment key={c.category} share={c.tokens / Math.max(used, 1)} background={c.color} />
            ))}
          </div>
        </div>
        <ul className="px-2 pb-2 pt-1.5">
          {cats.map((c) => (
            <li
              key={c.category}
              className={cn(
                "flex items-center gap-2.5 rounded-md px-2 py-1.5 text-[13px]",
                c.tokens === 0 && "text-muted-foreground",
              )}
            >
              <span className="size-2.5 shrink-0 rounded-[3px]" style={{ background: c.color }} aria-hidden />
              <span className="min-w-0 flex-1">
                {c.label}
                <span className="block truncate text-[11px] leading-4 text-muted-foreground">{c.hint}</span>
              </span>
              <span className="tabular-nums text-muted-foreground">{formatTokens(c.tokens)}</span>
              <span className="w-10 text-right font-medium tabular-nums">
                {formatPercent((c.tokens / Math.max(used, 1)) * 100)}
              </span>
            </li>
          ))}
        </ul>

        <p className="border-t px-4 py-2.5 text-[11px] leading-relaxed text-muted-foreground">
          直前のリクエスト時点（{usage.model}）。
          {usage.measured
            ? "合計はモデルの実測値、内訳は文字数からの推定です。"
            : "モデルが使用量を返さなかったため、合計も推定です。"}
          {window ? null : "このモデルはコンテキスト長が未設定のため、割合は出せません。"}
        </p>
      </PopoverContent>
    </Popover>
  );
}
