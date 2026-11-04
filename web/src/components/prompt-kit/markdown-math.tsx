// 回答本文の数式（#1015）。KaTeX 本体（約 260KB）と解析の拡張は、数式を含む本文が現れたときだけ読み込む。
"use client";

import * as React from "react";
import type { Options } from "react-markdown";
// 式の見た目（約 23KB）。字形のフォントは式を描いたときだけブラウザが取りに行く。
import "katex/dist/katex.min.css";

type PluggableList = NonNullable<Options["remarkPlugins"]>;

/// 行頭から始まるフェンスとその中身。閉じ行は CommonMark どおり、最大 3 文字の字下げを許し、
/// 開始と同じ文字で同じ長さ以上・後ろは空白だけ（`` ` `` と `~` を取り違えない）。閉じが無ければ末尾まで。
/// `` ` `` と `~` を 1 つの正規表現で文書の順に見る（別々に見ると、`~~~` の中の `` ``` `` 行を開始と取り違える）。
const TOP_LEVEL_FENCE =
  /^(?:(`{3,})[^`\n]*\n[\s\S]*?(?:^ {0,3}\1`*[ \t]*$|(?![\s\S]))|(~{3,})[^\n]*\n[\s\S]*?(?:^ {0,3}\2~*[ \t]*$|(?![\s\S])))/gm;

/// 本文に数式の候補（`$$`・`\(`・`\[`）があるか。読み込むかどうかにだけ使う判定なので、
/// 迷う所は「ある」に倒す（見落とすと数式が描かれない。多めに読み込んでも遅くなるだけ）。
/// 除くのは行頭から始まるフェンス（トップレベルのコードブロック）の中だけ（シェルの `$$` などで
/// KaTeX を読み込まない）。引用・リストの中のフェンスは暗黙に閉じることがあり、どこまでがコードかを
/// 正規表現では決められないので除かない。インラインコードも、閉じの対応を誤ると本文ごと消すので除かない。
export function mayHaveMath(text: string): boolean {
  const candidate = /\$\$|\\\(|\\\[/;
  if (!candidate.test(text)) return false;
  const prose = text.replace(TOP_LEVEL_FENCE, "");
  return candidate.test(prose);
}

type Katex = typeof import("katex").default;
type MathKit = { remark: PluggableList; katex: Katex };

let kit: MathKit | null = null;
let loading: Promise<MathKit> | null = null;

function loadMath(): Promise<MathKit> {
  loading ??= Promise.all([import("remark-math-extended"), import("katex")]).then(
    ([remarkMath, katex]) => {
      kit = {
        // `$$ … $$` に加え、LLM が書く `\( … \)`（行内）・`\[ … \]`（段落の頭からの別行立て）を
        // 解析器の段階で式として切り出す（コード・リスト・引用・表の構造は解析器が決める）。
        // 単独の `$ … $` は数式にしない（`$5 と $10` のような金額を数式に化けさせない）。
        remark: [[remarkMath.default, { singleDollarTextMath: false }]],
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
/// （そのあいだ本文は数式なしで描く）。
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
    return <span className="block overflow-x-auto overflow-y-hidden" dangerouslySetInnerHTML={{ __html: html }} />;
  }
  return <InlineMath html={html} />;
});

/// 行内の式。自分の箱の外には描かせない（\rlap 等で前後の本文や引用番号に重ねて描けないように）。
/// clip-path は overflow と違い、行内の箱のベースライン（本文との縦位置）を変えない。
/// 本文の幅より広い式（区切れない長い式）だけは、切り取らずに横スクロールにする。
function InlineMath({ html }: { html: string }) {
  const ref = React.useRef<HTMLSpanElement>(null);
  const [wide, setWide] = React.useState(false);
  React.useLayoutEffect(() => {
    const el = ref.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const check = () => setWide(el.scrollWidth > el.clientWidth + 1);
    check();
    const ro = new ResizeObserver(check);
    ro.observe(el);
    return () => ro.disconnect();
  }, [html]);
  return (
    <span
      ref={ref}
      className={
        wide
          ? "inline-block max-w-full overflow-x-auto overflow-y-hidden align-middle"
          : "inline-block max-w-full [clip-path:inset(0)]"
      }
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}

/// hast の code 要素のクラスが数式か（remark-math は行内を math-inline、別行立てを math-display にする）。
export function mathKind(className: string | undefined): "inline" | "display" | null {
  if (!className) return null;
  if (/(?:^|\s)math-display(?:\s|$)/.test(className)) return "display";
  if (/(?:^|\s)math-inline(?:\s|$)/.test(className)) return "inline";
  return null;
}
