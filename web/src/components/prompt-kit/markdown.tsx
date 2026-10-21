// prompt-kit (https://www.prompt-kit.com) 由来の Markdown/Response を本リポジトリ向けに実装。
// MIT License。react-markdown + remark-gfm で LLM 応答を描画する。コードブロックは CodeBlock（```ui は図・#530）、
// 装飾はすべてセマンティックトークンで行う（生色禁止）。
"use client";

import * as React from "react";
import dynamic from "next/dynamic";
import Link from "next/link";
import ReactMarkdown, { type Components, type ExtraProps } from "react-markdown";
import remarkGfm from "remark-gfm";

import { cn } from "@/lib/utils";
import { citeNumberFromHref } from "@/lib/citation";
import { CitationMarker } from "@/components/chat/citations/citation-marker";
import { resolveKind } from "@imanect/yui";
import { isYuiLanguage } from "@imanect/yui/react";

import { CodeBlock } from "./code-block";
// 引用・箇条書きの中の図はボタンを出さない（重い図の描画部とは別に、軽いコンテキストだけ読む）。
import { YuiNestedContext } from "./yui-actions";

/// ```ui の図は描画部（@imanect/yui の SVG レンダラー・書き出し）が重いので、図が現れたときだけ
/// 読み込む。読み込み中は図と同じくらいの高さの枠を置いてレイアウトが跳ねないようにする。
const UiFigure = dynamic(() => import("./ui-figure").then((m) => m.UiFigure), {
  ssr: false,
  loading: () => (
    <div className="my-3 h-48 w-full animate-pulse rounded-xl border border-border/60 bg-secondary/40" />
  ),
});

/// 長い URL・英数字の連なりで折り返す（はみ出すと会話全体が横にずれ、吹き出しの左端が切れる）。
const WRAP_ANYWHERE = "[overflow-wrap:anywhere]";

/// LLM は GFM 表のセル内改行に `<br>` を使うことがある（表セルは実改行を持てないため）。
/// react-markdown は raw HTML を通さず `<br>` が文字列で残り「安っぽく」見えるので、
/// テキストノード中の `<br>` / `<br/>` だけを mdast の break ノードに変換する。
/// 他の HTML は一切通さないため XSS の増分リスクは無い（安全な最小対処）。
function remarkBrTags() {
  const BR = /<br\s*\/?>/i;
  const ONLY_BR = /^<br\s*\/?>$/i;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const walk = (node: any) => {
    if (!node || !Array.isArray(node.children)) return;
    for (let i = 0; i < node.children.length; i++) {
      const child = node.children[i];
      // remark は `<br>` を inline HTML（type: "html"）として持つので break ノードへ差し替える。
      if (child.type === "html" && ONLY_BR.test((child.value ?? "").trim())) {
        node.children.splice(i, 1, { type: "break" });
        continue;
      }
      // 念のためテキスト中に紛れた `<br>` も分割して break を挟む。
      if (child.type === "text" && BR.test(child.value)) {
        const parts = child.value.split(BR);
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        const replacement: any[] = [];
        parts.forEach((p: string, idx: number) => {
          if (p) replacement.push({ type: "text", value: p });
          if (idx < parts.length - 1) replacement.push({ type: "break" });
        });
        node.children.splice(i, 1, ...replacement);
        i += replacement.length - 1;
      } else {
        walk(child);
      }
    }
  };
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (tree: any) => walk(tree);
}

/// 1 つの React 要素（```pre > code```）からコード文字列と言語を取り出す。
/// `raw` は react-markdown が渡す中身そのもの（フェンスが閉じていなくても末尾に改行が足されている）。
function extractCode(child: React.ReactNode): {
  code: string;
  raw: string;
  lang?: string;
  ui: boolean;
  /// ```ui:bar のように情報文字列で指定された図の種類。
  uiKind?: string;
} {
  if (!React.isValidElement(child)) return { code: "", raw: "", ui: false };
  const props = child.props as { className?: string; children?: React.ReactNode };
  const className = props.className ?? "";
  const match = /language-(\w+)/.exec(className);
  const raw = String(props.children ?? "");
  return {
    code: raw.replace(/\n$/, ""),
    raw,
    lang: match?.[1],
    ui: isYuiLanguage(className),
    uiKind: /language-(?:ui|yui):(\S+)/.exec(className)?.[1],
  };
}

/// 描画中の本文と、生成中かどうか（```ui の図が書きかけの行を描かないようにする）。
const SourceContext = React.createContext<{ text: string; streaming: boolean }>({
  text: "",
  streaming: false,
});

/// フェンスが原文で終わっているか。react-markdown（mdast-util-to-hast）はフェンスの中身の末尾に
/// 必ず改行を足すため、中身からは閉じたかどうかも、最終行が書き終わったかどうかも分からない。
/// 閉じ行がある場合のほか、後ろに本文が続く場合（引用・リストの終わりで暗黙に閉じた）も終わっている。
function fenceEnded(text: string, node: ExtraProps["node"]): boolean {
  const start = node?.position?.start.offset;
  const end = node?.position?.end.offset;
  if (start === undefined || end === undefined) return false;
  if (end < text.trimEnd().length) return true;
  const lines = text.slice(start, end).split(/\r?\n/);
  // 引用・リストの中のフェンスは閉じ行に `>` が付く。
  return lines.length > 1 && /^\s*(?:>\s*)*(`{3,}|~{3,})\s*$/.test(lines[lines.length - 1] ?? "");
}

/// ```ui bar のように空白区切りで書かれた種類（react-markdown はクラス名に載せず meta に残す）。
function fenceMeta(node: ExtraProps["node"]): string | undefined {
  const code = node?.children?.[0];
  const meta = code && "data" in code ? (code.data as { meta?: unknown } | undefined)?.meta : undefined;
  return typeof meta === "string" ? meta.trim().split(/\s+/)[0] : undefined;
}

/// 図の「表で表示」。リンク・画像は描かない（図と同じく文字と数値だけにする）。
/// ノートの ```ui の図（NodeView）も同じ描き方にする。
export function renderFigureTable(markdown: string) {
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      components={components}
      allowedElements={["p", "strong", "em", "br", "table", "thead", "tbody", "tr", "th", "td"]}
      unwrapDisallowed
    >
      {markdown}
    </ReactMarkdown>
  );
}

/// ブロックコード。```ui（Yui）の図は図にし、描けなければ通常のコードブロックに戻す（#530）。
function FencedCode({ node, children }: { node: ExtraProps["node"]; children: React.ReactNode }) {
  const { text, streaming } = React.useContext(SourceContext);
  const { code, raw, lang, ui, uiKind } = extractCode(children);
  const closed = fenceEnded(text, node);
  // 生成中でフェンスがまだ閉じていないコードはハイライトしない（トークンごとに shiki を回さない）。
  if (!ui) return <CodeBlock code={code} lang={lang} pending={streaming && !closed} />;
  // 終わっていないブロックは、確定した行だけを渡す（本文が改行で終わっていれば最終行も確定）。
  // 生成中は書きかけの行が伸びるたびに図を解析し直さないため、生成後（停止・閉じ忘れ）は
  // 途中で切れた値を確定値として描かないため。
  const lastLineDone = closed || text.endsWith("\n");
  const source = lastLineDone ? raw : raw.slice(0, raw.lastIndexOf("\n", raw.length - 2) + 1);
  const kind = uiKind ?? fenceMeta(node);
  return (
    <UiFigure
      source={source}
      // 記述そのものを見せるのは書き終わった後だけ。生成中は渡さない（トークンごとに描き直さない）。
      raw={closed || !streaming ? raw : undefined}
      complete={closed || !streaming}
      closed={closed}
      truncated={!lastLineDone && !streaming && raw.trim() !== source.trim()}
      kind={kind ? resolveKind(kind) : undefined}
      lang={lang}
      renderTable={renderFigureTable}
    />
  );
}

const components: Components = {
  // ブロックコードは pre をフックして CodeBlock に差し替える。
  pre({ node, children }) {
    return <FencedCode node={node}>{children}</FencedCode>;
  },
  code({ className, children }) {
    // インラインコード（pre 配下は上で処理済み）。
    return (
      <code
        className={cn(
          "rounded-[5px] bg-muted px-1.5 py-0.5 font-mono text-[0.85em] text-foreground",
          WRAP_ANYWHERE,
          className,
        )}
      >
        {children}
      </code>
    );
  },
  a({ href, children }) {
    const url = href ?? "";
    // 引用マーカー（`#cite-n`・linkifyCitations が付ける）は本文中の上付き番号チップにする。
    // ホバーで原文カード、クリックで出典パネル（メッセージの引用文脈があるときのみ）。
    const cite = citeNumberFromHref(url);
    if (cite != null) return <CitationMarker n={cite}>{children}</CitationMarker>;
    // その他の内部リンクはクライアント遷移。
    if (url.startsWith("/")) {
      return (
        <Link
          href={url}
          className={cn(
            "font-medium text-primary underline decoration-primary/30 underline-offset-2 hover:decoration-primary",
            WRAP_ANYWHERE,
          )}
        >
          {children}
        </Link>
      );
    }
    return (
      <a
        href={url}
        target="_blank"
        rel="noopener noreferrer"
        className={cn(
          "font-medium text-primary underline decoration-primary/30 underline-offset-2 hover:decoration-primary",
          WRAP_ANYWHERE,
        )}
      >
        {children}
      </a>
    );
  },
  p({ children }) {
    return <p className="my-2.5 leading-relaxed first:mt-0 last:mb-0">{children}</p>;
  },
  ul({ children }) {
    return <ul className="my-2.5 ml-5 list-disc space-y-1 marker:text-muted-foreground">{children}</ul>;
  },
  ol({ children }) {
    return <ol className="my-2.5 ml-5 list-decimal space-y-1 marker:text-muted-foreground">{children}</ol>;
  },
  li({ children }) {
    // 箇条書きの中の図はボタンを出さない（操作を持つ図はトップレベルだけ・サーバーと同じ規則）。
    return (
      <li className="leading-relaxed">
        <YuiNestedContext.Provider value={true}>{children}</YuiNestedContext.Provider>
      </li>
    );
  },
  h1({ children }) {
    return <h1 className="mb-2 mt-4 text-xl font-semibold first:mt-0">{children}</h1>;
  },
  h2({ children }) {
    return <h2 className="mb-2 mt-4 text-lg font-semibold first:mt-0">{children}</h2>;
  },
  h3({ children }) {
    return <h3 className="mb-1.5 mt-3 text-base font-semibold first:mt-0">{children}</h3>;
  },
  blockquote({ children }) {
    return (
      <blockquote className="my-3 border-l-2 border-border pl-4 text-muted-foreground">
        <YuiNestedContext.Provider value={true}>{children}</YuiNestedContext.Provider>
      </blockquote>
    );
  },
  hr() {
    return <hr className="my-4 border-border" />;
  },
  // 表は横スクロールの箱に収める（本文の幅を押し広げない）。見出しは折り返さず、セルは
  // 最小幅を持たせて 1 文字ずつの縦長にならないようにする。箱の端まで来た横スクロールが
  // ページ（戻るジェスチャ）へ抜けないよう overscroll-x-contain。
  table({ children }) {
    return (
      <div className="my-3 max-w-full overflow-x-auto overscroll-x-contain rounded-lg border border-border">
        <table className="w-full border-collapse text-[13px]">{children}</table>
      </div>
    );
  },
  thead({ children }) {
    return <thead className="bg-muted/50">{children}</thead>;
  },
  th({ children }) {
    return (
      <th className="whitespace-nowrap border-b border-border px-3 py-2 text-left font-semibold">
        {children}
      </th>
    );
  },
  td({ children }) {
    return (
      <td className="min-w-[7rem] border-b border-border/60 px-3 py-2 [overflow-wrap:break-word]">
        {children}
      </td>
    );
  },
  strong({ children }) {
    return <strong className="font-semibold text-foreground">{children}</strong>;
  },
};

export function Markdown({
  children,
  className,
  streaming = false,
}: {
  children: string;
  className?: string;
  /// 生成中の本文か（```ui の図が書きかけの行を描かないようにする）。
  streaming?: boolean;
}) {
  return (
    <div className={cn("min-w-0 text-[15px] leading-relaxed text-foreground", WRAP_ANYWHERE, className)}>
      <SourceContext.Provider value={{ text: children, streaming }}>
        <ReactMarkdown remarkPlugins={[remarkGfm, remarkBrTags]} components={components}>
          {children}
        </ReactMarkdown>
      </SourceContext.Provider>
    </div>
  );
}
