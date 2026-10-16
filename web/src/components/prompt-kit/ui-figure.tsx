// ```ui ブロック（Yui・#530）を図にして描く。チャットの回答の Markdown の中に置かれる。
// 解析・描画は @imanect/yui に任せる（文字と数値だけを描き、HTML・URL・スクリプトを持たない）。
// 生成途中のブロックは、呼び出し側が確定した行だけを渡す（書きかけの値は描かない）。
"use client";

import * as React from "react";
import { BarChart3, Check, Copy, Download, Table2 } from "lucide-react";
import { isEmpty, parseBlock, toText, type Kind } from "@imanect/yui";
import { copyFigure, downloadFigure } from "@imanect/yui/export";
import { YuiFigure, diagnosticsSummary } from "@imanect/yui/react";

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import { CodeBlock } from "./code-block";
import { useYuiActions, YuiRunList } from "./yui-actions";

/// 生成中の本文はトークンごとに再描画されるので、props が変わらない限り描き直さない
/// （確定済みの図・書きかけの行しか変わっていない図の解析とレイアウトを繰り返さない）。
export const UiFigure = React.memo(function UiFigure({
  source,
  raw,
  complete,
  closed,
  truncated,
  kind,
  lang,
  renderTable,
  actions,
}: {
  /// 図の記述（確定した行だけ。complete なら全体）。
  source: string;
  /// ブロックが書き終わっているか（フェンスが閉じた・生成完了）。
  complete: boolean;
  /// フェンスが閉じ行で閉じているか（省略時は complete と同じ）。閉じていないブロックはサーバーが
  /// 操作の対象にしないので、ボタンを押せなくする。
  closed?: boolean;
  /// 生成が途中で終わり（停止・閉じ忘れ）、書きかけの最終行を落としたか。
  truncated?: boolean;
  /// 図にできないときにそのまま見せる記述（書きかけの最終行も含む全体）。省略時は source。
  raw?: string;
  /// フェンスの情報文字列で指定された種類（```ui:bar）。
  kind?: Kind;
  /// 図にできないときに表示するコードブロックの言語名。
  lang?: string;
  /// 「表で表示」の中身（テキスト化した Markdown）を描く。
  renderTable: (markdown: string) => React.ReactNode;
  /// 図の操作欄の末尾に足すボタン（ノートの「ソース」切り替えなど）。
  actions?: React.ReactNode;
}) {
  const result = React.useMemo(
    () => parseBlock(source, kind ? { final: complete, kind } : { final: complete }),
    [source, complete, kind],
  );
  const figure = result.figure;
  // 書き終わっても描けない（種類が分からない・データが 1 件も無い）ときは、記述をそのまま見せる。
  const failed = complete && (!figure || result.diagnostics.some((d) => d.severity === "error"));
  const text = React.useMemo(() => (figure && !failed ? toText(figure) : ""), [figure, failed]);
  const [asTable, setAsTable] = React.useState(false);
  const [copied, setCopied] = React.useState(false);
  // 操作を持つ図（app・ask・plan）のボタン。確定した回答の中でだけ押せる（#604）。
  const actions_ = useYuiActions(failed ? null : figure, source, complete, closed ?? complete);

  if (failed) {
    return (
      <div data-testid="ui-figure-fallback">
        <CodeBlock code={(raw ?? source).replace(/\n$/, "")} lang={lang} />
        <p className="-mt-1.5 mb-3 text-[11px] text-muted-foreground">
          図にできなかったため、記述をそのまま表示しています。
        </p>
      </div>
    );
  }
  if (!figure || isEmpty(figure)) {
    // 生成中で、まだ描ける行が届いていない。見出しだけの空の図は出さない。
    return (
      <div
        className="my-3 flex h-24 items-center justify-center rounded-xl border border-dashed border-border bg-secondary/30 text-xs text-muted-foreground"
        data-testid="ui-figure-pending"
      >
        図を作成しています…
      </div>
    );
  }

  const note = complete
    ? [
        truncated ? "生成が途中で終わったため、最後の行は表示していません" : null,
        diagnosticsSummary(result.diagnostics),
      ]
        .filter(Boolean)
        .join("。")
    : null;
  const copy = async () => {
    try {
      // 画像（PNG）・画像と表の HTML・表と箇条書きのテキスト・Yui のソースを一度に入れる。
      // 貼り付け先が形式を選ぶ（Word やメールは画像、Slack やエディタは表）。画像を書けない
      // 環境では、テキストだけが入る。
      await copyFigure(figure);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      /* クリップボード不可は無視 */
    }
  };
  // 書き出す図は明るい配色で色を焼き込む（画面がダークでも、文書に貼る前提の見た目にする）。
  const save = (format: "png" | "svg") => {
    void downloadFigure(figure, format).catch(() => {
      /* 保存できない環境は無視 */
    });
  };

  return (
    <figure className="my-3 min-w-0" data-testid="ui-figure" data-ui-kind={figure.kind}>
      {asTable ? (
        <div className="text-[14px]">{renderTable(text)}</div>
      ) : (
        // 図の内側の余白（幅 400px 未満は 12px、それ以上は 16px）だけ外へ広げ、見出しを本文の左端に揃える。
        <YuiFigure
          figure={figure}
          className="-mx-3 sm:-mx-4"
          {...(actions_?.onAction ? { onAction: actions_.onAction } : {})}
          {...(actions_?.actionDisabledReason ? { actionDisabledReason: actions_.actionDisabledReason } : {})}
          {...(actions_?.invokedActions ? { invokedActions: actions_.invokedActions } : {})}
        />
      )}
      {actions_ ? <YuiRunList runs={actions_.runs} /> : null}
      <figcaption
        className={cn(
          "mt-1 flex items-center justify-end gap-1 text-[11px] text-muted-foreground",
          // 足したボタン（ノートの「ソース」）で狭い幅に収まらないときだけ折り返す（チャットは従来どおり）。
          actions != null && "flex-wrap",
        )}
      >
        {note ? <span className="mr-auto">{note}</span> : null}
        {complete ? (
          <>
            <ToolbarButton onClick={() => setAsTable((v) => !v)} pressed={asTable}>
              {asTable ? <BarChart3 className="size-3.5" /> : <Table2 className="size-3.5" />}
              {asTable ? "図で表示" : "表で表示"}
            </ToolbarButton>
            <ToolbarButton onClick={copy} label="図を画像と表としてコピー">
              {copied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
              {copied ? "コピー済み" : "コピー"}
            </ToolbarButton>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <ToolbarButton label="図を画像で保存">
                  <Download className="size-3.5" />
                  保存
                </ToolbarButton>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="min-w-[9rem]">
                <DropdownMenuItem onSelect={() => save("png")}>PNG で保存</DropdownMenuItem>
                <DropdownMenuItem onSelect={() => save("svg")}>SVG で保存</DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </>
        ) : null}
        {actions}
      </figcaption>
    </figure>
  );
});

/// メニューの開閉ボタン（Radix の asChild）にも使うので、ref と残りの属性をそのまま渡す。
/// ノートの図（```ui の NodeView）の切り替えボタンも同じ見た目にするため公開する。
export function ToolbarButton({
  pressed,
  label,
  className,
  children,
  ...props
}: React.ComponentProps<"button"> & {
  pressed?: boolean;
  label?: string;
}) {
  return (
    <button
      type="button"
      aria-pressed={pressed}
      aria-label={label}
      className={cn(
        "flex items-center gap-1 rounded-md px-1.5 py-1 transition-colors hover:bg-secondary hover:text-foreground",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
        "data-[state=open]:bg-secondary data-[state=open]:text-foreground",
        className,
      )}
      {...props}
    >
      {children}
    </button>
  );
}
