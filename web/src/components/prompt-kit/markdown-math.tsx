// 回答本文の数式（#1015）。KaTeX 本体（約 260KB）は数式を含む本文が現れたときだけ読み込む。
"use client";

import * as React from "react";
import type { Options } from "react-markdown";
// 式の見た目（約 23KB）。字形のフォントは式を描いたときだけブラウザが取りに行く。
import "katex/dist/katex.min.css";

type PluggableList = NonNullable<Options["remarkPlugins"]>;

// LLM が書く `\( … \)`・`\[ … \]` は、Markdown の解析より前に remark-math の `$$ … $$` へ置き換える
// （Markdown として解析させると `\\`・`\{`・`_` が先に解釈されて式が壊れるため、解析器に式として切り出させる）。
// 置き換えた区切りには私用領域の印を内側に付ける。解析の後で、
// - 式（inlineMath）からは印を外し、`\[` 由来なら別行立てにする。
// - コード（code / inlineCode）など式にならなかった所では、印ごと元の `\(`・`\[` に戻す。
// 解析器が構造（リスト・引用・表・コード）を決めるので、置き換え側でそれらを判定しない。
const OPEN_INLINE = "";
const CLOSE_INLINE = "";
const OPEN_DISPLAY = "";
const CLOSE_DISPLAY = "";
const MARKS = /[-]/g;

/// 本文に数式の候補（`$$`・`\(`・`\[`）があるか。フェンスとインラインコードの中は数えない
/// （シェルの `$$` などで KaTeX を読み込まない）。判定は読み込むかどうかにだけ使うので近似でよい。
export function mayHaveMath(text: string): boolean {
  if (!/\$\$|\\\(|\\\[/.test(text)) return false;
  const prose = text.replace(/(`{3,}|~{3,})[\s\S]*?(?:\1|$)/g, "").replace(/(`+)[\s\S]*?\1/g, "");
  return /\$\$|\\\(|\\\[[\s\S]*?[\\^_={}][\s\S]*?\\\]/.test(prose);
}

/// `\( … \)`・`\[ … \]` を印付きの `$$ … $$` にする。
///
/// 次の 2 つのときは、`$$` の前後に幅 0 の空白を挟む（解析器に別の意味に読まれないように）:
/// - 行の頭（引用の `>`・リストの記号の後を含む）。`$$` で始まる行は数式のブロックの開始とみなされる。
/// - 隣が `$` か別の区切り（`\(a\)\(b\)`）。`$$` の連なりがつながって区切りがずれる。
/// `\[注1\]` のような角括弧のエスケープは残す（式らしい記号を含むか、複数行のときだけ式にする）。
export function markMathDelimiters(text: string): string {
  return text.replace(
    /\\\[([\s\S]+?)\\\]|\\\(([\s\S]+?)\\\)/g,
    (m, display: string | undefined, inline: string | undefined, offset: number, all: string) => {
      if (display !== undefined && !/[\\^_={}\n]/.test(display)) return m;
      const body =
        display !== undefined
          ? `$$${OPEN_DISPLAY}${display}${CLOSE_DISPLAY}$$`
          : `$$${OPEN_INLINE}${inline}${CLOSE_INLINE}$$`;
      const lineHead = all.slice(all.lastIndexOf("\n", offset - 1) + 1, offset);
      const atLineStart = /^[\s>]*(?:(?:[-*+]|\d+[.)])\s+)?$/.test(lineHead);
      const before = all.slice(Math.max(0, offset - 2), offset);
      const after = all.slice(offset + m.length, offset + m.length + 2);
      const sepBefore = atLineStart || before.endsWith("$") || /\\[)\]]$/.test(before);
      const sepAfter = after.startsWith("$") || /^\\[([]/.test(after);
      return `${sepBefore ? "\u200B" : ""}${body}${sepAfter ? "\u200B" : ""}`;
    },
  );
}

/// 印を元の区切りに戻す（式にならなかった所）。
function unmark(s: string): string {
  return s
    .replaceAll(`$$${OPEN_INLINE}`, "\\(")
    .replaceAll(`${CLOSE_INLINE}$$`, "\\)")
    .replaceAll(`$$${OPEN_DISPLAY}`, "\\[")
    .replaceAll(`${CLOSE_DISPLAY}$$`, "\\]")
    .replace(MARKS, "");
}

const DISPLAY_CLASS = ["language-math", "math-display"];

/// 解析後の印の後始末（remark-math の後に置く）。
function remarkMathMarks() {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const walk = (node: any, parent: any) => {
    if (node.type === "inlineMath") {
      const display = node.value.includes(OPEN_DISPLAY);
      node.value = node.value.replace(MARKS, "").trim();
      // remark-math は描画用の中身（hChildren）を解析時に写しているので、そちらも差し替える。
      node.data = { ...node.data, hChildren: [{ type: "text", value: node.value }] };
      if (display) {
        node.data.hProperties = { className: DISPLAY_CLASS };
        // 段落に式だけがあるなら、段落ごと別行立ての式にする（`$$` のブロックと同じ描き方）。
        const onlyChild =
          parent?.type === "paragraph" &&
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          parent.children.every((c: any) => c === node || (c.type === "text" && /^[\s\u200B]*$/.test(c.value)));
        if (onlyChild) parent.__displayMath = node.value;
      }
      return;
    }
    if (typeof node.value === "string") node.value = unmark(node.value);
    if (typeof node.url === "string") node.url = unmark(node.url);
    if (Array.isArray(node.children)) {
      for (const c of node.children) walk(c, node);
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      node.children = node.children.map((c: any) =>
        c.__displayMath === undefined
          ? c
          : {
              type: "math",
              value: c.__displayMath,
              position: c.position,
              data: {
                hName: "pre",
                hChildren: [
                  {
                    type: "element",
                    tagName: "code",
                    properties: { className: DISPLAY_CLASS },
                    children: [{ type: "text", value: c.__displayMath }],
                  },
                ],
              },
            },
      );
    }
  };
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (tree: any) => walk(tree, null);
}

type Katex = typeof import("katex").default;
type MathKit = { remark: PluggableList; katex: Katex };

let kit: MathKit | null = null;
let loading: Promise<MathKit> | null = null;

function loadMath(): Promise<MathKit> {
  loading ??= Promise.all([import("remark-math"), import("katex")]).then(
    ([remarkMath, katex]) => {
      kit = {
        remark: [[remarkMath.default, { singleDollarTextMath: false }], remarkMathMarks],
        katex: katex.default,
      };
      return kit;
    },
    (e: unknown) => {
      // 一時的な読み込み失敗で以後ずっと描けなくならないよう、次に数式が現れたときに読み直す。
      loading = null;
      throw e;
    },
  );
  return loading;
}

/// 数式の解析プラグイン。数式の候補が無い本文では読み込まない。読み込むまでは null
/// （そのあいだ本文は置き換えずに原文のまま描く）。
export function useMathPlugins(enabled: boolean): PluggableList | null {
  const [ready, setReady] = React.useState<MathKit | null>(kit);
  React.useEffect(() => {
    if (!enabled || ready) return;
    let alive = true;
    loadMath().then(
      (k) => alive && setReady(k),
      () => {
        /* 読み込めなければ原文のまま */
      },
    );
    return () => {
      alive = false;
    };
  }, [enabled, ready]);
  return enabled && ready ? ready.remark : null;
}

/// 描いた式の HTML（式 → HTML）。生成中は本文が伸びるたびに全体を描き直すため、同じ式を
/// KaTeX に何度も通さない。上限を超えたら古いものから捨てる。
const CACHE_MAX = 500;
const cache = new Map<string, string>();

function renderTex(tex: string, display: boolean): string {
  const key = `${display ? "D" : "I"}${tex}`;
  const hit = cache.get(key);
  if (hit !== undefined) return hit;
  if (!kit) return "";
  const html = kit.katex.renderToString(tex, {
    displayMode: display,
    // 書き損じた式はその式だけ赤字で見せ、回答全体を壊さない。
    throwOnError: false,
    strict: "ignore",
    // 本文は LLM・他人が書いたものなので信頼しない: \href 等は通さず（trust: false）、
    // \rule などの大きさに上限を設け、マクロの展開回数も絞る。
    trust: false,
    maxSize: 10,
    maxExpand: 500,
  });
  if (cache.size >= CACHE_MAX) cache.delete(cache.keys().next().value!);
  cache.set(key, html);
  return html;
}

/// 1 つの式。KaTeX の出力（trust: false・入力はエスケープ済み）を HTML として差し込む。
/// 式ごとに memo するので、生成中に本文が伸びても書き終わった式は描き直さない。
export const MathTex = React.memo(function MathTex({ tex, display }: { tex: string; display: boolean }) {
  const html = renderTex(tex, display);
  if (display) {
    return (
      <span
        className="block overflow-x-auto overflow-y-hidden"
        dangerouslySetInnerHTML={{ __html: html }}
      />
    );
  }
  // 行内の式は自分の箱の外に描かせない（\rlap 等で前後の本文や引用番号に重ねて描けないように）。
  // clip-path は overflow と違い、行内の箱のベースライン（本文との縦位置）を変えない。
  return (
    <span
      className="inline-block max-w-full [clip-path:inset(0)]"
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
});

/// hast の code 要素のクラスが数式か（remark-math は行内を math-inline、別行立てを math-display にする）。
export function mathKind(className: string | undefined): "inline" | "display" | null {
  if (!className) return null;
  if (/(?:^|\s)math-display(?:\s|$)/.test(className)) return "display";
  if (/(?:^|\s)math-inline(?:\s|$)/.test(className)) return "inline";
  return null;
}
