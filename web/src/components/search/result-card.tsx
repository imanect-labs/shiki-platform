"use client";

import { useMemo, useState } from "react";
import Link from "next/link";
import { ChevronDown, ChevronRight, ExternalLink, FileText } from "lucide-react";

import { cn } from "@/lib/utils";
import type { SearchResult } from "@/lib/search";

/// クエリ語の単純部分一致ハイライト（<mark>）。
/// 形態素は使わず、空白区切りトークンの長い順に一致させる（引用の当たりを掴む用途）。
function highlight(text: string, query: string): React.ReactNode {
  const tokens = query
    .split(/\s+/)
    .map((t) => t.trim())
    // 英数は 2 文字以上、CJK は 1 文字でも有効な検索語（「税」「法」等）として扱う。
    .filter((t) => t.length >= 2 || /[\u3040-\u30ff\u3400-\u9fff々]/.test(t))
    .sort((a, b) => b.length - a.length);
  if (tokens.length === 0) return text;
  const escaped = tokens.map((t) => t.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  const splitter = new RegExp(`(${escaped.join("|")})`, "g");
  // 判定は /g を持たない別インスタンスで行う（lastIndex 汚染を避ける）。
  const matcher = new RegExp(`^(?:${escaped.join("|")})$`);
  const parts = text.split(splitter);
  return parts.map((part, i) =>
    matcher.test(part) ? (
      <mark key={i} className="rounded-sm bg-primary/15 px-0.5 text-foreground">
        {part}
      </mark>
    ) : (
      <span key={i}>{part}</span>
    ),
  );
}

/// 検索結果 1 件（引用チャンク）。
export function ResultCard({ result, query }: { result: SearchResult; query: string }) {
  const [showParent, setShowParent] = useState(false);
  const highlighted = useMemo(() => highlight(result.content, query), [result.content, query]);

  const driveHref = result.folder_id ? `/drive?folder=${result.folder_id}` : "/drive";

  return (
    <article className="group rounded-xl border border-border bg-card p-4 shadow-sm transition-shadow hover:shadow-md">
      {/* ヘッダ: ファイル名 → 見出しパス → ページ */}
      <div className="mb-2 flex items-start justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2">
          <FileText className="size-4 shrink-0 text-primary" aria-hidden />
          <div className="min-w-0">
            <p className="truncate text-sm font-semibold text-foreground">{result.file_name}</p>
            {result.heading_path.length > 0 ? (
              <p className="mt-0.5 flex items-center gap-1 truncate text-xs text-muted-foreground">
                {result.heading_path.map((h, i) => (
                  <span key={i} className="flex items-center gap-1">
                    {i > 0 ? <ChevronRight className="size-3 shrink-0" aria-hidden /> : null}
                    <span className="truncate">{h}</span>
                  </span>
                ))}
              </p>
            ) : null}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {result.page != null ? (
            <span className="rounded-full border border-border bg-muted/50 px-2 py-0.5 text-[11px] text-muted-foreground">
              p.{result.page}
            </span>
          ) : null}
          <Link
            href={driveHref}
            className="inline-flex items-center gap-1 rounded-md px-2 py-1 text-xs text-muted-foreground opacity-0 transition-opacity hover:bg-muted hover:text-foreground group-hover:opacity-100"
            title="ドライブで開く"
          >
            <ExternalLink className="size-3.5" aria-hidden />
            ドライブ
          </Link>
        </div>
      </div>

      {/* 引用本文（クエリ語ハイライト） */}
      <p className="whitespace-pre-wrap text-sm leading-relaxed text-foreground/90">
        {highlighted}
      </p>

      {/* フッタ: スコアバー ＋ 親文脈の展開 */}
      <div className="mt-3 flex items-center justify-between gap-3">
        <div
          className="flex items-center gap-2"
          title={`関連度スコア: ${result.score.toFixed(3)}`}
        >
          <div className="h-1.5 w-24 overflow-hidden rounded-full bg-muted">
            <div
              className="h-full rounded-full bg-primary"
              style={{ width: `${Math.round(Math.min(1, Math.max(0, result.score)) * 100)}%` }}
            />
          </div>
          <span className="text-[11px] tabular-nums text-muted-foreground">
            {result.score.toFixed(2)}
          </span>
        </div>
        {result.parent_content ? (
          <button
            type="button"
            onClick={() => setShowParent((v) => !v)}
            className="inline-flex items-center gap-1 text-xs text-muted-foreground transition-colors hover:text-foreground"
            aria-expanded={showParent}
          >
            <ChevronDown
              className={cn("size-3.5 transition-transform", showParent && "rotate-180")}
              aria-hidden
            />
            前後の文脈
          </button>
        ) : null}
      </div>
      {showParent && result.parent_content ? (
        <div className="mt-3 rounded-lg border border-border bg-muted/30 p-3 text-xs leading-relaxed text-muted-foreground">
          {result.parent_content}
        </div>
      ) : null}
    </article>
  );
}
