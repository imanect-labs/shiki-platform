"use client";

/// 検証済み UI スペックのレンダラ（Task 6.6）。
///
/// **信頼カタログの React 実装への静的マッピングのみ**を行う:
/// `dangerouslySetInnerHTML`・`eval`・動的 import は使わない。描画対象はサーバの検証層を
/// 通過したスペックだが、フロントは**防御的に縮退**する — 未知コンポーネント/壊れた props は
/// クラッシュさせずプレースホルダ表示に落とす（Task 6.6 受け入れ条件）。

import * as React from "react";

import { ExternalLink, Puzzle } from "lucide-react";

import type { UiNode, UiSpecDoc } from "@/generated/gui-spec";
import { cn } from "@/lib/utils";
import { GenUiForm } from "./form";
import { GenUiButton } from "./gen-button";
import { GenUiTable } from "./gen-table";
import { GenUiChart } from "./gen-chart";
import { GenUiStat } from "./gen-stat";

/// レンダラの深さ上限（サーバ検証の MAX_DEPTH=8 に余裕を足した防御値）。
const MAX_RENDER_DEPTH = 10;

/// スペック（unknown）を最低限の構造チェックで UiSpecDoc として解釈する。
/// サーバ検証済みが前提のため型ガードは浅く、深い検証はしない（縮退は NodeView 側）。
export function parseSpec(spec: unknown): UiSpecDoc | null {
  if (typeof spec !== "object" || spec === null) return null;
  const doc = spec as Partial<UiSpecDoc>;
  if (typeof doc.version !== "number" || typeof doc.root !== "object" || doc.root === null) {
    return null;
  }
  return { version: doc.version, actions: doc.actions ?? [], root: doc.root as UiNode };
}

/// generative UI ブロックのルート。カード状の枠に収めてチャット/アプリ双方で使う。
export function SpecRenderer({ spec, className }: { spec: unknown; className?: string }) {
  const doc = React.useMemo(() => parseSpec(spec), [spec]);
  if (!doc) return <UnknownComponent label="ui" />;
  return (
    <div
      data-testid="genui-root"
      className={cn(
        "my-2 rounded-xl border border-border bg-card p-4 shadow-[var(--elevation-1,0_1px_2px_rgb(0_0_0/0.06))]",
        className,
      )}
    >
      <NodeView node={doc.root} depth={0} />
    </div>
  );
}

/// カタログ → React 実装の静的マッピング（switch＝閉じた集合）。
export function NodeView({ node, depth }: { node: UiNode; depth: number }) {
  if (depth > MAX_RENDER_DEPTH) return <UnknownComponent label="…" />;
  // 防御: 型不明のオブジェクトが混ざってもクラッシュさせない。
  if (typeof node !== "object" || node === null || typeof node.component !== "string") {
    return <UnknownComponent label="unknown" />;
  }
  switch (node.component) {
    case "container":
      return (
        <section className="min-w-0">
          {node.title ? (
            <h3 className="mb-2 text-[13px] font-semibold tracking-wide text-foreground/80">
              {node.title}
            </h3>
          ) : null}
          <div
            className={cn(
              "flex min-w-0 gap-3",
              node.layout === "horizontal" ? "flex-row flex-wrap items-start" : "flex-col",
            )}
          >
            {(node.children ?? []).map((child, i) => (
              <NodeView key={i} node={child} depth={depth + 1} />
            ))}
          </div>
        </section>
      );
    case "text": {
      const variant =
        node.variant === "heading"
          ? "text-[15px] font-semibold text-foreground"
          : node.variant === "caption"
            ? "text-xs text-muted-foreground"
            : "text-sm leading-relaxed text-foreground/90";
      // プレーンテキストのみ（markdown/HTML は解釈しない・改行のみ反映）。
      return <p className={cn("whitespace-pre-wrap", variant)}>{node.text}</p>;
    }
    case "link":
      // https のみ（サーバ検証済み）。防御的に再チェックし、外部リンクとして開く。
      if (!node.href?.startsWith("https://")) return <UnknownComponent label="link" />;
      return (
        <a
          href={node.href}
          target="_blank"
          rel="noopener noreferrer"
          className="inline-flex items-center gap-1 text-sm font-medium text-primary underline-offset-4 hover:underline"
        >
          {node.text}
          <ExternalLink className="size-3.5" aria-hidden />
        </a>
      );
    case "form":
      return <GenUiForm form={node} />;
    case "button":
      return <GenUiButton button={node} />;
    case "table":
      return <GenUiTable table={node} />;
    case "chart":
      return <GenUiChart spec={node} />;
    case "stat":
      return <GenUiStat stat={node} />;
    default:
      // 予約 variant（map/image）や将来カタログはプレースホルダ縮退（クラッシュさせない）。
      return <UnknownComponent label={node.component} />;
  }
}

/// 未知/未対応コンポーネントの安全な縮退表示。
export function UnknownComponent({ label }: { label: string }) {
  return (
    <div
      data-testid="genui-unknown"
      className="flex items-center gap-2 rounded-lg border border-dashed border-border bg-secondary/40 px-3 py-2 text-xs text-muted-foreground"
    >
      <Puzzle className="size-3.5 shrink-0" aria-hidden />
      このコンポーネント（{label}）はまだ表示できません
    </div>
  );
}
