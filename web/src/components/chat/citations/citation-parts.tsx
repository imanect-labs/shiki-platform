"use client";

/// 引用 UI の小さな共通部品（文書アイコン・種別・ハイライト・番号チップ・原本リンク）。
import * as React from "react";
import Link from "next/link";
import { ExternalLink } from "lucide-react";

import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import { matchSpan } from "@/lib/citation";
import { resourcePath } from "@/lib/resource-link";
import type { NodeMeta } from "@/lib/node-name-cache";
import { NodeIcon } from "@/components/drive/primitives";

/// 文書の色（四季トークンの巡回）を CSS 変数 `--doc` として注入する。
export function docColorStyle(colorIndex: number | undefined): React.CSSProperties {
  return { ["--doc" as string]: seasonVar(colorIndex ?? 0) } as React.CSSProperties;
}

export function DocIcon({ meta, className }: { meta: NodeMeta | undefined; className?: string }) {
  return (
    <NodeIcon
      kind="file"
      name={meta?.name ?? ""}
      contentType={meta?.contentType}
      className={cn("size-4 shrink-0", className)}
    />
  );
}

const KIND_LABELS: [RegExp, string][] = [
  [/\.pdf$/i, "PDF"],
  [/\.(docx?|odt|rtf)$/i, "Word"],
  [/\.(xlsx?|ods)$/i, "Excel"],
  [/\.(pptx?|odp)$/i, "PowerPoint"],
  [/\.(md|markdown)$/i, "ノート"],
  [/\.csv$/i, "CSV"],
  [/\.slide$/i, "スライド"],
  [/\.(txt|log)$/i, "テキスト"],
  [/\.html?$/i, "HTML"],
];

export function kindLabel(name: string | undefined): string | null {
  if (!name) return null;
  return KIND_LABELS.find(([re]) => re.test(name))?.[1] ?? null;
}

export function KindBadge({ name }: { name: string | undefined }) {
  const label = kindLabel(name);
  if (!label) return null;
  return (
    <span className="shrink-0 rounded-[4px] bg-muted px-1 py-px text-[10px] font-medium leading-normal text-muted-foreground">
      {label}
    </span>
  );
}

/// 更新日（YYYY/MM/DD）。
export function formatDate(iso: string | null | undefined): string | null {
  if (!iso) return null;
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return null;
  return `${d.getFullYear()}/${String(d.getMonth() + 1).padStart(2, "0")}/${String(d.getDate()).padStart(2, "0")}`;
}

/// 「原本で開く」の遷移先と文言。専用エディタの無い形式は格納フォルダを開く。
export function openTarget(nodeId: string, meta: NodeMeta | undefined): { href: string; label: string } | null {
  if (!meta) return null;
  const href = resourcePath({ id: nodeId, name: meta.name, kind: "file", parent_id: meta.parentId });
  if (href.startsWith("/notes/")) return { href, label: "ノートで開く" };
  if (href.startsWith("/office/")) return { href, label: "Office で開く" };
  if (href.startsWith("/csv/")) return { href, label: "表で開く" };
  if (href.startsWith("/slides/")) return { href, label: "スライドで開く" };
  return { href, label: "フォルダを開く" };
}

export function OpenOriginalLink({
  nodeId,
  meta,
  variant = "ghost",
  className,
}: {
  nodeId: string;
  meta: NodeMeta | undefined;
  variant?: "ghost" | "outline";
  className?: string;
}) {
  const target = openTarget(nodeId, meta);
  if (!target) return null;
  return (
    <Link
      href={target.href}
      className={cn(
        "inline-flex shrink-0 items-center gap-1 rounded-lg px-2 py-1 text-[12px] transition-colors",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
        variant === "outline"
          ? "border border-border bg-card text-foreground/80 hover:bg-muted"
          : "text-muted-foreground hover:bg-muted hover:text-foreground",
        className,
      )}
    >
      {target.label}
      <ExternalLink className="size-3" aria-hidden />
    </Link>
  );
}

/// 抜粋を描き、主張に対応する 1 文をハイライトする。`around` を渡すと、ハイライトの前を
/// その文字数に詰める（行数を絞った表示でハイライトが見切れないように）。
export function SnippetText({
  text,
  claim,
  around,
  className,
}: {
  text: string;
  claim?: string;
  around?: number;
  className?: string;
}) {
  const trimmed = text.trim();
  // 一覧・対照表は生成中にトークンごとに再描画されるので、照合（bigram 集合の構築）は覚えておく。
  const span = React.useMemo(() => matchSpan(trimmed, claim), [trimmed, claim]);
  if (!span) return <span className={className}>{trimmed}</span>;
  const [s, e] = span;
  const before = around != null && s > around ? `…${trimmed.slice(s - around, s)}` : trimmed.slice(0, s);
  return (
    <span className={className}>
      {before}
      <mark className="rounded-[3px] bg-[var(--season-autumn)]/20 px-0.5 text-foreground [box-decoration-break:clone]">
        {trimmed.slice(s, e)}
      </mark>
      {trimmed.slice(e)}
    </span>
  );
}

/// 一覧・パネル・対照表で使う番号バッジ（本文のチップと同じ文書色）。
export function NumberBadge({
  n,
  colorIndex,
  solid = false,
  className,
}: {
  n: number;
  colorIndex: number | undefined;
  solid?: boolean;
  className?: string;
}) {
  return (
    <span
      style={docColorStyle(colorIndex)}
      className={cn(
        "flex size-[18px] shrink-0 items-center justify-center rounded-[5px] text-[10.5px] font-semibold tabular-nums",
        solid ? "bg-[var(--doc)] text-background" : "bg-[var(--doc)]/15 text-[var(--doc)]",
        className,
      )}
    >
      {n}
    </span>
  );
}
