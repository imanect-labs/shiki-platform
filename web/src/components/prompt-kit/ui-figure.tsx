// ```ui ブロック（Yui・#530）を図にして描く。チャットの回答の Markdown の中に置かれる。
// 解析・描画は @imanect/yui に任せる（文字と数値だけを描き、HTML・URL・スクリプトを持たない）。
// 生成途中は改行で終わっていない最終行を読まないので、値が書きかけのまま出ることはない。
"use client";

import * as React from "react";
import { BarChart3, Check, Copy, Table2 } from "lucide-react";
import { parseBlock, toText } from "@imanect/yui";
import { YuiFigure, diagnosticsSummary } from "@imanect/yui/react";

import { cn } from "@/lib/utils";

export function UiFigure({
  source,
  streaming,
  fallback,
  renderMarkdown,
}: {
  /// フェンスの中身（閉じたフェンスなら末尾の改行を含む）。
  source: string;
  /// 生成中か。true なら書きかけの最終行を読まない。
  streaming: boolean;
  /// 図にできないとき（生成完了後）に出すもの。
  fallback: React.ReactNode;
  /// 「表で表示」の中身（テキスト化した Markdown）を描く。
  renderMarkdown: (markdown: string) => React.ReactNode;
}) {
  const result = React.useMemo(() => parseBlock(source, { final: !streaming }), [source, streaming]);
  const [asTable, setAsTable] = React.useState(false);
  const [copied, setCopied] = React.useState(false);
  const figure = result.figure;

  if (!figure) {
    if (!streaming) return <>{fallback}</>;
    return (
      <div
        className="my-3 flex h-24 items-center justify-center rounded-xl border border-dashed border-border bg-secondary/30 text-xs text-muted-foreground"
        data-testid="ui-figure-pending"
      >
        図を作成しています…
      </div>
    );
  }

  const text = toText(figure);
  const note = streaming ? null : diagnosticsSummary(result.diagnostics);
  const copy = async () => {
    try {
      // 貼り付け先を選ばないよう、表・箇条書きにしたテキストを入れる。
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      /* クリップボード不可は無視 */
    }
  };

  return (
    <figure className="my-3 min-w-0" data-testid="ui-figure" data-ui-kind={figure.kind}>
      {asTable ? <div className="text-[14px]">{renderMarkdown(text)}</div> : <YuiFigure figure={figure} />}
      <figcaption className="mt-1 flex items-center justify-end gap-1 text-[11px] text-muted-foreground">
        {note ? <span className="mr-auto">{note}</span> : null}
        {streaming ? null : (
          <>
            <ToolbarButton onClick={() => setAsTable((v) => !v)} pressed={asTable}>
              {asTable ? <BarChart3 className="size-3.5" /> : <Table2 className="size-3.5" />}
              {asTable ? "図で表示" : "表で表示"}
            </ToolbarButton>
            <ToolbarButton onClick={copy} label="図の内容をテキストでコピー">
              {copied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
              {copied ? "コピー済み" : "コピー"}
            </ToolbarButton>
          </>
        )}
      </figcaption>
    </figure>
  );
}

function ToolbarButton({
  onClick,
  pressed,
  label,
  children,
}: {
  onClick: () => void;
  pressed?: boolean;
  label?: string;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={pressed}
      aria-label={label}
      className={cn(
        "flex items-center gap-1 rounded-md px-1.5 py-1 transition-colors hover:bg-secondary hover:text-foreground",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
      )}
    >
      {children}
    </button>
  );
}
