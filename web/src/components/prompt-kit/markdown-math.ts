// 回答本文の数式（#1015）。KaTeX は重いので、数式を含む本文が現れたときだけ読み込む。
"use client";

import * as React from "react";
import type { Options } from "react-markdown";
// 式の見た目（約 23KB）。字形のフォントは式を描いたときだけブラウザが取りに行く。重い JS（KaTeX 本体）は下で遅延読み込み。
import "katex/dist/katex.min.css";

type PluggableList = NonNullable<Options["remarkPlugins"]>;

/// LLM が書く `\( … \)`・`\[ … \]` を remark-math の `$$` に揃える（コードの中は触らない）。
///
/// - `\( x \)` → `$$x$$`（行内の数式）
/// - `\[ x \]` → 前後に改行を置いた `$$` のブロック（別行立ての数式）
///
/// 単独の `$ … $` は数式にしない（`$5 と $10` のような金額を数式に化けさせない）。
/// フェンス（```）とインラインコード（`…`）の中身はそのまま残す。
export function normalizeMathDelimiters(text: string): string {
  if (!text.includes("\\(") && !text.includes("\\[")) return text;
  const lines = text.split("\n");
  const out: string[] = [];
  let fence: string | null = null;
  let buf: string[] = [];
  const flush = () => {
    if (buf.length) out.push(convertProse(buf.join("\n")));
    buf = [];
  };
  for (const line of lines) {
    const m = /^\s*(?:>\s*)*(`{3,}|~{3,})/.exec(line);
    if (fence === null && m) {
      flush();
      fence = m[1]!;
      out.push(line);
    } else if (fence !== null) {
      out.push(line);
      if (m && m[1]!.startsWith(fence) && /^\s*(?:>\s*)*(`{3,}|~{3,})\s*$/.test(line)) fence = null;
    } else {
      buf.push(line);
    }
  }
  flush();
  return out.join("\n");
}

/// フェンスの外の本文。インラインコードを避けて区切り記号を置き換える。
function convertProse(prose: string): string {
  return prose
    .split(/(`+[^`]*`+)/)
    .map((part, i) =>
      i % 2 === 1
        ? part
        : part
            // `\[注1\]` のような角括弧のエスケープは残す（式らしい記号を含むか、複数行のときだけ式にする）。
            .replace(/\\\[([\s\S]+?)\\\]/g, (all, body: string) =>
              /[\\^_={}\n]/.test(body) ? `\n$$\n${body.trim()}\n$$\n` : all,
            )
            .replace(/\\\(([\s\S]+?)\\\)/g, (_, body: string) => `$$${body.trim()}$$`),
    )
    .join("");
}

/// 本文に数式（`$$`）があるか。
export function hasMath(text: string): boolean {
  return text.includes("$$");
}

type MathPlugins = { remark: PluggableList; rehype: PluggableList };

let loaded: MathPlugins | null = null;
let loading: Promise<MathPlugins> | null = null;

function loadMathPlugins(): Promise<MathPlugins> {
  loading ??= Promise.all([import("remark-math"), import("rehype-katex")]).then(
    ([remarkMath, rehypeKatex]) => {
      loaded = {
        remark: [[remarkMath.default, { singleDollarTextMath: false }]],
        // 書き損じた式はその式だけ赤字で見せ、回答全体を壊さない。trust は既定の false（\href 等を通さない）。
        rehype: [[rehypeKatex.default, { throwOnError: false, strict: "ignore" }]],
      };
      return loaded;
    },
  );
  return loading;
}

/// 数式を描くプラグイン。数式が無い本文では読み込まない。読み込むまでは null（原文のまま描く）。
export function useMathPlugins(enabled: boolean): MathPlugins | null {
  const [plugins, setPlugins] = React.useState<MathPlugins | null>(loaded);
  React.useEffect(() => {
    if (!enabled || plugins) return;
    let alive = true;
    loadMathPlugins().then(
      (p) => alive && setPlugins(p),
      () => {
        /* 読み込めなければ原文のまま */
      },
    );
    return () => {
      alive = false;
    };
  }, [enabled, plugins]);
  return enabled ? plugins : null;
}
