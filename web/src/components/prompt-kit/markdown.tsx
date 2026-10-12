// prompt-kit (https://www.prompt-kit.com) 由来の Markdown/Response を本リポジトリ向けに実装。
// MIT License。react-markdown + remark-gfm で LLM 応答を描画する。コードブロックは CodeBlock（```ui は図・#530）、
// 装飾はすべてセマンティックトークンで行う（生色禁止）。
"use client";

import * as React from "react";
import Link from "next/link";
import ReactMarkdown, { type Components, type ExtraProps } from "react-markdown";
import remarkGfm from "remark-gfm";

import { cn } from "@/lib/utils";
import { citeNumberFromHref } from "@/lib/citation";
import { CitationMarker } from "@/components/chat/citations/citation-marker";
import { resolveKind } from "@imanect/yui";
import { isYuiLanguage } from "@imanect/yui/react";

import { CodeBlock } from "./code-block";
import { UiFigure } from "./ui-figure";

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

/// フェンスが原文で閉じているか。react-markdown（mdast-util-to-hast）はフェンスの中身の末尾に
/// 必ず改行を足すため、中身からは閉じたかどうかも、最終行が書き終わったかどうかも分からない。
function fenceClosed(text: string, node: ExtraProps["node"]): boolean {
  const start = node?.position?.start.offset;
  const end = node?.position?.end.offset;
  if (start === undefined || end === undefined) return false;
  const lines = text.slice(start, end).split(/\r?\n/);
  return lines.length > 1 && /^\s*(`{3,}|~{3,})\s*$/.test(lines[lines.length - 1] ?? "");
}

/// 図の「表で表示」。リンク・画像は描かない（図と同じく文字と数値だけにする）。
function renderFigureTable(markdown: string) {
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
  if (!ui) return <CodeBlock code={code} lang={lang} />;
  const complete = !streaming || fenceClosed(text, node);
  // 書き終わっていないブロックは、確定した行だけを渡す（本文が改行で終わっていれば最終行も確定）。
  // 書きかけの行が伸びるたびに図を解析し直さないため、ここで切っておく。
  const source = complete ? raw : text.endsWith("\n") ? raw : raw.slice(0, raw.lastIndexOf("\n", raw.length - 2) + 1);
  return (
    <UiFigure
      source={source}
      complete={complete}
      kind={uiKind ? resolveKind(uiKind) : undefined}
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
          className="font-medium text-primary underline decoration-primary/30 underline-offset-2 hover:decoration-primary"
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
        className="font-medium text-primary underline decoration-primary/30 underline-offset-2 hover:decoration-primary"
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
    return <li className="leading-relaxed">{children}</li>;
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
        {children}
      </blockquote>
    );
  },
  hr() {
    return <hr className="my-4 border-border" />;
  },
  table({ children }) {
    return (
      <div className="my-3 overflow-x-auto rounded-lg border border-border">
        <table className="w-full border-collapse text-[13px]">{children}</table>
      </div>
    );
  },
  thead({ children }) {
    return <thead className="bg-muted/50">{children}</thead>;
  },
  th({ children }) {
    return <th className="border-b border-border px-3 py-2 text-left font-semibold">{children}</th>;
  },
  td({ children }) {
    return <td className="border-b border-border/60 px-3 py-2">{children}</td>;
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
    <div className={cn("text-[15px] leading-relaxed text-foreground", className)}>
      <SourceContext.Provider value={{ text: children, streaming }}>
        <ReactMarkdown remarkPlugins={[remarkGfm, remarkBrTags]} components={components}>
          {children}
        </ReactMarkdown>
      </SourceContext.Provider>
    </div>
  );
}
