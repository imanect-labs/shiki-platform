/// 引用（doc_search のソース）の解釈を一か所にまとめる（issue #505）。
///
/// - 本文の `[n]` → 引用の解決は `citationAt` だけが行う。サーバが振った応答内の通し番号
///   （`cite_id`）で引く（#508）。採番前に保存された旧データ（`cite_id` が 0）だけは、従来どおり
///   「メッセージ内の引用を到着順に並べた配列の n-1 番目」で引く。
/// - 表示は「引用箇所（チャンク）ごとの番号」を保ったまま、一覧だけを文書ごとにまとめる。
/// - 回答の文（主張）と原文の対応は、本文中の `[n]` の直前の 1 文から推定する。
import type { Citation } from "@/lib/chat-api";

/// 本文中の引用マーカーのリンク先。Markdown の a レンダラがこの接頭辞で引用チップに差し替える。
export const CITE_HREF_PREFIX = "#cite-";

/// サーバが番号を振った引用か（旧データは 0 / 欠落）。
function hasCiteIds(citations: readonly Citation[]): boolean {
  return citations.some((c) => (c.cite_id ?? 0) > 0);
}

/// 配列の i 番目の引用が本文で何番として書かれているか。
export function numberOf(citations: readonly Citation[], i: number): number {
  const id = citations[i]?.cite_id ?? 0;
  return id > 0 ? id : i + 1;
}

/// 本文の `[n]` に対応する引用（無ければ undefined）。
export function citationAt(citations: readonly Citation[], n: number): Citation | undefined {
  if (!Number.isInteger(n) || n < 1) return undefined;
  if (hasCiteIds(citations)) return citations.find((c) => c.cite_id === n && !c.withheld);
  return citations[n - 1];
}

/// `[n]` が「閲覧できない出典」か。共有された会話で閲覧者の権限では読めない文書の引用は、
/// サーバが番号だけの引用（`withheld`）に置き換えて返す（番号は詰めない）。
export function isWithheldCitation(citations: readonly Citation[], n: number): boolean {
  return citations.some((c) => c.withheld && c.cite_id === n);
}

/// 表示に使う引用（閲覧できない出典を除く）。
export function visibleCitations(citations: readonly Citation[]): Citation[] {
  return citations.filter((c) => !c.withheld);
}

/// 本文中の `[n]` 引用マーカーを、引用チップ用のリンクに変換する（Markdown 用）。
/// 対応する引用が無く、欠けた番号でもない `[n]` はそのまま残す。
///
/// コード（フェンスのコードブロック・インラインコード）の中は変えない。Markdown の解析前に
/// 文字列で置き換えるので、ここで除かないとコード例の `[1]` が `[1](#cite-1)` に化け、表示と
/// コピー結果が壊れる。
export function linkifyCitations(text: string, citations: readonly Citation[]): string {
  if (citations.length === 0) return text;
  return splitByCode(text)
    .map(({ code, s }) =>
      code
        ? s
        : s.replace(CITE_RUN, (run: string) =>
            run.replace(/\[(\d+)\]/g, (match, digits: string) => {
              const n = Number.parseInt(digits, 10);
              if (!citationAt(citations, n) && !isWithheldCitation(citations, n)) return match;
              return `[${n}](${CITE_HREF_PREFIX}${n})`;
            }),
          ),
    )
    .join("");
}

/// 本文中の `[n]` の並び（`[1][3]` は 1 まとまり）。既存の Markdown リンクの一部は除く:
/// `[1](url)`・画像 `![1](x)`・参照リンク `[本文][1]` / `[1][ref]`・参照定義 `[1]: url`
/// （ラベルを引用に化けさせない）。
const CITE_RUN = /(?<![\]!])(?:\[\d+\])+(?![(\[:])/g;

/// コード部分の範囲（`[start, end)`・文字列の添字）。
function codeRanges(text: string): [number, number][] {
  const out: [number, number][] = [];
  let at = 0;
  for (const part of splitByCode(text)) {
    if (part.code) out.push([at, at + part.s.length]);
    at += part.s.length;
  }
  return out;
}

/// 本文をコード（```〜``` / ~~~〜~~~ のフェンス、`〜` のインラインコード）とそれ以外に分ける。
/// 閉じていないフェンス（生成途中）は末尾までコードとして扱う。
export function splitByCode(text: string): { code: boolean; s: string }[] {
  const out: { code: boolean; s: string }[] = [];
  const re = /(^|\n)([ \t]*)(`{3,}|~{3,})[^\n]*(?:\n[\s\S]*?(?:\n[ \t]*\3[`~]*[ \t]*(?=\n|$))|[\s\S]*$)|(`+)[^`\n]+?\4/g;
  let last = 0;
  for (let m = re.exec(text); m; m = re.exec(text)) {
    // フェンスの前の改行は本文側に残す。
    const start = m.index + (m[1]?.length ?? 0);
    if (start > last) out.push({ code: false, s: text.slice(last, start) });
    out.push({ code: true, s: text.slice(start, m.index + m[0].length) });
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push({ code: false, s: text.slice(last) });
  return out;
}

/// 引用チップのリンク先から番号を取り出す（引用リンクでなければ null）。
export function citeNumberFromHref(href: string): number | null {
  if (!href.startsWith(CITE_HREF_PREFIX)) return null;
  const n = Number.parseInt(href.slice(CITE_HREF_PREFIX.length), 10);
  return Number.isFinite(n) ? n : null;
}

/// 文の区切り（日本語の句点・感嘆/疑問符・改行）。
const SENTENCE_END = /[。！？!?\n]/;

/// Markdown の装飾記号を落として地の文にする（主張の表示・照合用）。
function plain(s: string): string {
  return s
    .replace(/\[(\d+)\]/g, "")
    .replace(/[*_`#>~|]/g, "")
    .replace(/^\s*(?:[-+]|\d+\.)\s+/gm, "")
    .replace(/\s+/g, " ")
    .trim();
}

/// 出典を示すだけの行（「【出典】…」「出典: …」「参照：…」）。
const SOURCE_LABEL = /^[【\[(（]?\s*(?:出典|参照|引用元|根拠)\s*[】\])）:：]/;

/// 主張として見せる長さの上限（文字）。長い本文は末尾側を残す（番号に近い方が根拠に近い）。
const CLAIM_MAX = 240;

function clip(s: string): string {
  const chars = Array.from(s);
  return chars.length > CLAIM_MAX ? `…${chars.slice(chars.length - CLAIM_MAX).join("")}` : s;
}

/// 連続した引用マーカー（`[3][4]` など）1 まとまりと、その直前の 1 文（主張）。
export type CitationRun = { ns: number[]; claim: string };

/// 本文から引用マーカーのまとまりを出現順に取り出す。範囲外の番号は捨てる。
export function citationRuns(text: string, citations: readonly Citation[]): CitationRun[] {
  const runs: CitationRun[] = [];
  // linkifyCitations と同じ判定（Markdown リンクのラベルは引用として数えない）。
  const re = new RegExp(CITE_RUN.source, "g");
  // コードの中の `[n]` は引用ではない（linkifyCitations もリンクにしない）。
  const code = codeRanges(text);
  let prevEnd = 0;
  // 直前の出典行の本文（「【出典】…[3]」「【出典】…[4]」と出典行が続くとき、後ろの行も同じ本文を指す）。
  let lastLabelBody = "";
  for (let m = re.exec(text); m; m = re.exec(text)) {
    const at = m.index;
    if (code.some(([s, e]) => at >= s && at < e)) continue;
    const ns = Array.from(m[0].matchAll(/\[(\d+)\]/g), (x) => Number.parseInt(x[1], 10)).filter(
      (n) => citationAt(citations, n),
    );
    // 直前の文の先頭 = 直前の区切り文字の次、ただし前のマーカーより前には戻らない。
    let start = m.index;
    while (start > prevEnd && !SENTENCE_END.test(text[start - 1])) start--;
    let raw = text.slice(start, m.index);
    // マーカーの直前が句点で終わる書き方（「〜できます。[1]」）は、その句点までの 1 文を取る。
    if (!plain(raw)) {
      let s = m.index - 1;
      while (s > prevEnd && SENTENCE_END.test(text[s])) s--;
      let b = s;
      while (b > prevEnd && !SENTENCE_END.test(text[b - 1])) b--;
      raw = text.slice(b, s + 1);
    }
    // 「【出典】就業規則 第32条[1]」のように、本文の後に出典行を立てて番号を付ける書き方がある。
    // その行は主張ではないので、前の番号から出典行までの本文を主張とする。
    let body: string | null = null;
    if (SOURCE_LABEL.test(plain(raw))) {
      body = clip(plain(text.slice(prevEnd, start)));
      if (!body) body = lastLabelBody;
      lastLabelBody = body;
    } else {
      lastLabelBody = "";
    }
    prevEnd = m.index + m[0].length;
    // 「〜である[1]、[2]。」のように番号の間に句読点しか無い場合は、直前のまとまりに合流する。
    const claim = body ?? plain(raw).replace(/^[、，,。．.\s]+/, "");
    if (ns.length === 0) continue;
    const last = runs[runs.length - 1];
    if (!claim && last) last.ns.push(...ns.filter((n) => !last.ns.includes(n)));
    else runs.push({ ns, claim });
  }
  return runs;
}

/// 引用箇所（同じチャンクが複数回返った場合は番号をまとめる）。
export type CitedPassage = {
  /// 代表番号（本文で最初に使われた番号。未使用なら最初の番号）。一覧の番号表示とパネルの送り順に使う。
  n: number;
  /// この箇所を指す番号（昇順）。
  ns: number[];
  citation: Citation;
  /// 本文で実際に引用されたか。
  used: boolean;
};

/// 文書ごとにまとめた引用。
export type CitedDocument = {
  nodeId: string;
  passages: CitedPassage[];
};

export type CitationGroups = {
  /// 回答で使われた引用を、文書の初出順にまとめたもの。
  docs: CitedDocument[];
  /// 検索はしたが本文で引用されなかった箇所。
  unused: CitedPassage[];
  /// 本文に引用マーカーが 1 つでもあるか。無ければ全引用を「参照した」として docs に入れる。
  hasMarkers: boolean;
  /// node_id → 文書の色番号（四季の巡回）。本文のチップと一覧で同じ色を使う。
  colorOf: Record<string, number>;
  /// 本文で引用された番号の集合。
  usedNumbers: Set<number>;
};

/// 引用を文書ごとにまとめ、本文で使われたものと使われなかったものに分ける。
///
/// `runs` は `citationRuns(text, citations)` の結果（呼び出し側で計算済みなら渡して二重走査を避ける）。
export function groupCitations(
  citations: readonly Citation[],
  text: string,
  runs: readonly CitationRun[] = citationRuns(text, citations),
): CitationGroups {
  const usedNumbers = new Set<number>();
  for (const r of runs) for (const n of r.ns) usedNumbers.add(n);
  const hasMarkers = usedNumbers.size > 0;

  // 同じチャンクは 1 箇所にまとめる（2 回の検索で同じ結果が返ることがある）。
  const byChunk = new Map<string, CitedPassage>();
  const order: CitedPassage[] = [];
  citations.forEach((c, i) => {
    // 閲覧できない出典は一覧に出さない（本文の番号だけを欠番として描く）。
    if (c.withheld) return;
    const n = numberOf(citations, i);
    const used = !hasMarkers || usedNumbers.has(n);
    const hit = byChunk.get(c.chunk_id);
    if (hit) {
      // 番号を振り直す前の旧データは別番号、振り直した後は同じ番号で再ヒットする。
      if (!hit.ns.includes(n)) hit.ns.push(n);
      hit.used ||= used;
      return;
    }
    const p: CitedPassage = { n, ns: [n], citation: c, used };
    byChunk.set(c.chunk_id, p);
    order.push(p);
  });

  // 文書の並び = 本文で最初に引用された順（マーカーが無ければ到着順）。
  const firstUse = (p: CitedPassage) => {
    if (!hasMarkers) return p.ns[0];
    const ns = p.ns.filter((n) => usedNumbers.has(n));
    return ns.length > 0 ? Math.min(...ns) : Number.POSITIVE_INFINITY;
  };
  for (const p of order) {
    const first = firstUse(p);
    p.n = Number.isFinite(first) ? first : p.ns[0];
  }
  const docMap = new Map<string, CitedDocument & { first: number }>();
  const unused: CitedPassage[] = [];
  for (const p of order) {
    if (!p.used) {
      unused.push(p);
      continue;
    }
    const id = p.citation.node_id;
    const doc = docMap.get(id) ?? { nodeId: id, passages: [], first: Number.POSITIVE_INFINITY };
    doc.passages.push(p);
    doc.first = Math.min(doc.first, firstUse(p));
    docMap.set(id, doc);
  }
  const docs = [...docMap.values()]
    .sort((a, b) => a.first - b.first)
    .map(({ nodeId, passages }) => ({
      nodeId,
      passages: passages.sort((a, b) => firstUse(a) - firstUse(b)),
    }));

  const colorOf: Record<string, number> = {};
  for (const d of docs) colorOf[d.nodeId] = Object.keys(colorOf).length;
  for (const p of unused) {
    const id = p.citation.node_id;
    if (!(id in colorOf)) colorOf[id] = Object.keys(colorOf).length;
  }
  return { docs, unused, hasMarkers, colorOf, usedNumbers };
}

/// 番号 → その番号を含む最初のまとまりの主張（本文の該当文）。
export function claimsByNumber(runs: readonly CitationRun[]): Map<number, string> {
  const out = new Map<number, string>();
  for (const r of runs) for (const n of r.ns) if (!out.has(n) && r.claim) out.set(n, r.claim);
  return out;
}

/// 見出しパスの末尾と、あればページ（一覧・カードの「場所」表示）。
export function citationLocator(c: Pick<Citation, "heading_path" | "page">): string {
  const last = c.heading_path && c.heading_path.length > 0 ? c.heading_path[c.heading_path.length - 1] : "";
  if (c.page != null) return last ? `p.${c.page} ・ ${last}` : `p.${c.page}`;
  return last;
}

function bigrams(s: string): Set<string> {
  const t = s.replace(/[\s、。，．,.「」『』（）()［］[\]・:：]/g, "");
  const out = new Set<string>();
  for (let i = 0; i + 1 < t.length; i++) out.add(t.slice(i, i + 2));
  return out;
}

/// 抜粋の中で、主張に最もよく対応する 1 文の範囲を返す（強調表示用）。
///
/// サーバは一致箇所を返さないため、文字 bigram の重なりで推定する。重なりが薄い
/// （主張の bigram の 3 割未満）なら強調しない＝誤った箇所を自信ありげに光らせない。
export function matchSpan(snippet: string, claim: string | undefined): [number, number] | null {
  if (!claim) return null;
  const want = bigrams(claim);
  if (want.size < 3) return null;
  let best: [number, number] | null = null;
  let bestScore = 0;
  let start = 0;
  for (let i = 0; i <= snippet.length; i++) {
    if (i < snippet.length && !SENTENCE_END.test(snippet[i])) continue;
    const end = Math.min(i + 1, snippet.length);
    const sentence = snippet.slice(start, end);
    const have = bigrams(sentence);
    let hit = 0;
    for (const b of want) if (have.has(b)) hit++;
    const score = hit / want.size;
    if (score > bestScore && sentence.trim()) {
      bestScore = score;
      // 先頭の空白・改行はハイライトに含めない。
      const lead = sentence.length - sentence.trimStart().length;
      best = [start + lead, end];
    }
    start = end;
  }
  if (bestScore < 0.3 || !best) return null;
  // 抜粋のほぼ全体が 1 文なら、強調しても情報が増えないので付けない。
  const body = snippet.trim().length;
  return best[1] - best[0] >= body * 0.9 ? null : best;
}
