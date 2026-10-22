"use client";

import * as React from "react";

import { cn } from "@/lib/utils";
import type { ContextCategory, ContextUsage } from "@/lib/chat-api";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

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

/// 窓に対する使用率（窓が分からなければ null）。
export function contextPercent(usage: ContextUsage): number | null {
  return usage.window_tokens ? (usage.used_tokens / usage.window_tokens) * 100 : null;
}

/// ⋯ メニューの項目名（「コンテキスト 12%」）。
export function contextMenuLabel(usage: ContextUsage): string {
  const p = contextPercent(usage);
  return `コンテキスト ${p === null ? formatTokens(usage.used_tokens) : formatPercent(p)}`;
}

/// 使用率に応じた色（余裕あり→注意→逼迫）。
function levelColor(percent: number | null): string {
  if (percent === null || percent < 70) return "var(--primary)";
  if (percent < 90) return "var(--season-autumn)";
  return "var(--destructive)";
}

type Part = { key: string; label: string; hint?: string; tokens: number; background: string };

/// 帯の区間（使用中の各カテゴリ・応答用の予約・空き）。窓が分からなければ使用中だけ。
function parts(usage: ContextUsage): { parts: Part[]; total: number } {
  const window = usage.window_tokens ?? null;
  const out: Part[] = usage.categories.map((c) => ({
    key: c.category,
    label: CATEGORY[c.category].label,
    hint: CATEGORY[c.category].hint,
    tokens: c.tokens,
    background: CATEGORY[c.category].color,
  }));
  if (!window) return { parts: out, total: Math.max(usage.used_tokens, 1) };
  const reserved = Math.min(usage.reserved_output_tokens ?? 0, window);
  out.push({
    key: "reserved",
    label: "応答用の予約",
    hint: "回答の出力に空けておく分",
    tokens: reserved,
    background: RESERVED_BG,
  });
  out.push({
    key: "free",
    label: "空き",
    tokens: Math.max(window - usage.used_tokens - reserved, 0),
    background: "var(--muted)",
  });
  return { parts: out, total: window };
}

/// コンテキストの使用量（何がどれだけ占めているか）。会話の ⋯ メニューから開く。
///
/// 値は**直前の LLM リクエスト**のもの（生成中はステップごとに更新される）。総量はモデルの実測、
/// 内訳は文字数からの見積りの比で按分したもの（サーバ側 `agent_core::context_usage`）。
///
/// 数字は帯・凡例にホバー（タッチではタップ、キーボードではフォーカス）すると帯の下に出る。
export function ContextUsageDialog({
  usage,
  open,
  onOpenChange,
}: {
  usage: ContextUsage | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const [active, setActive] = React.useState<string | null>(null);
  React.useEffect(() => {
    if (!open) setActive(null);
  }, [open]);
  if (!usage) return null;

  const percent = contextPercent(usage);
  const window = usage.window_tokens ?? null;
  const { parts: segs, total } = parts(usage);
  const focused = segs.find((s) => s.key === active) ?? null;
  const share = (t: number) => (t / total) * 100;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md gap-0">
        <DialogHeader>
          <DialogTitle>コンテキスト</DialogTitle>
          <DialogDescription className="tabular-nums">
            <span className="font-medium text-foreground">{formatTokens(usage.used_tokens)}</span>
            {window ? ` / ${formatTokens(window)}` : null} トークン使用
            {percent !== null ? (
              <span className="ml-1.5 font-semibold" style={{ color: levelColor(percent) }}>
                {formatPercent(percent)}
              </span>
            ) : null}
          </DialogDescription>
        </DialogHeader>

        {/* 帯（窓全体）。区間にホバー/タップ/フォーカスすると下の行に数字が出る。 */}
        <div
          className="mt-5 flex h-4 overflow-hidden rounded-md bg-muted"
          onMouseLeave={() => setActive(null)}
        >
          {segs
            .filter((s) => s.tokens > 0 && s.key !== "free")
            .map((s) => (
              <button
                key={s.key}
                type="button"
                aria-label={`${s.label} ${formatTokens(s.tokens)} トークン（${formatPercent(share(s.tokens))}）`}
                onMouseEnter={() => setActive(s.key)}
                onFocus={() => setActive(s.key)}
                onClick={() => setActive(s.key)}
                className={cn(
                  "h-full border-r-2 border-card transition-opacity last:border-r-0 focus-visible:outline-none",
                  active && active !== s.key && "opacity-40",
                )}
                style={{ width: `${share(s.tokens)}%`, minWidth: 6, background: s.background }}
              />
            ))}
          {/* 空きは帯の残り全体（押せる範囲を広く取る）。 */}
          {window ? (
            <button
              type="button"
              aria-label={`空き ${formatTokens(segs[segs.length - 1].tokens)} トークン`}
              onMouseEnter={() => setActive("free")}
              onFocus={() => setActive("free")}
              onClick={() => setActive("free")}
              className="h-full flex-1 focus-visible:outline-none"
            />
          ) : null}
        </div>

        {/* 数字（何も指していないときは案内）。2 行分の高さを固定して凡例の位置がずれないようにする。 */}
        <div className="mt-2.5 min-h-11 text-[13px] tabular-nums" aria-live="polite">
          {focused ? (
            <>
              <p className="font-medium">{focused.label}</p>
              <p className="flex flex-wrap gap-x-3 text-muted-foreground">
                <span className="whitespace-nowrap">
                  <span className="text-foreground">{focused.tokens.toLocaleString()}</span> トークン
                </span>
                {window ? (
                  <span className="whitespace-nowrap">
                    窓の <span className="text-foreground">{formatPercent(share(focused.tokens))}</span>
                  </span>
                ) : null}
                {focused.key in CATEGORY && usage.used_tokens > 0 ? (
                  <span className="whitespace-nowrap">
                    使用中の{" "}
                    <span className="font-semibold text-foreground">
                      {formatPercent((focused.tokens / usage.used_tokens) * 100)}
                    </span>
                  </span>
                ) : null}
              </p>
            </>
          ) : (
            <p className="pt-0.5 text-muted-foreground">
              帯や項目にカーソルを合わせる（スマホはタップ）と数字が出ます
            </p>
          )}
        </div>

        {/* 凡例（数字は出さない・ホバー/タップで上の行に出す）。 */}
        <ul className="-mx-2 mt-3 grid grid-cols-2 gap-0.5">
          {segs.map((s) => (
            <li key={s.key}>
              <button
                type="button"
                onMouseEnter={() => setActive(s.key)}
                onMouseLeave={() => setActive(null)}
                onFocus={() => setActive(s.key)}
                onClick={() => setActive(s.key)}
                className={cn(
                  "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-[13px] transition-colors pointer-coarse:py-2.5",
                  "hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                  active === s.key && "bg-accent",
                  s.tokens === 0 && "text-muted-foreground",
                )}
              >
                <span
                  className={cn("size-2.5 shrink-0 rounded-[3px]", s.key === "free" && "border border-border")}
                  style={{ background: s.background }}
                  aria-hidden
                />
                <span className="truncate">{s.label}</span>
              </button>
            </li>
          ))}
        </ul>

        <p className="mt-4 border-t pt-3 text-[11px] leading-relaxed text-muted-foreground">
          直前のリクエスト時点（{usage.model}）。
          {usage.measured
            ? "合計はモデルの実測値、内訳は文字数からの推定です。"
            : "モデルが使用量を返さなかったため、合計も推定です。"}
          {window ? null : "このモデルはコンテキスト長が未設定のため、割合は出せません。"}
        </p>
      </DialogContent>
    </Dialog>
  );
}
