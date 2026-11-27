/// ミニアプリの画面（Yui の ```ui を含む Markdown）を、組み立て用の「段」の並びに分けて、つなぎ直す。
///
/// - 文章の段・app の段（組み立てで直せる）・表の段（```data_grid・束ねたビューを描く・GAP-11）・そのままの段
///   （ほかの図・直せない app）の 4 種類。
/// - 表の段にするのは、行頭（字下げなし）の閉じた ```data_grid（~~~ も）で、ほかのコードブロックの中でないものだけ。
///   字下げのあるもの（箇条書き・引用の中かもしれない）は文章の段のまま残す（読み違えて段を作らない）。
/// - 何も変えなければ、つなぎ直した文字列は元と 1 文字も変わらない（`splitMarkdown` の行の区切りを
///   そのまま戻すため）。並べ替え・追加で段が新しく隣り合ったときだけ、間に空行を補う。
/// - app の段を直したときは、その段の中身だけを `toSource` の正規形に書き直す。フェンスの行と字下げは残す。

import { splitMarkdown, toSource, type AppFigure, type Diagnostic } from "@imanect/yui";
import { checkEditable } from "@imanect/yui/editor";

/// `pos` は元の画面での並び（隣り合っていた段はつなぎ目をそのまま戻す）。新しい段には無い。
export type ScreenBlock = (
  | {
      id: string;
      type: "text";
      /// 段の中身（前後の空行を除く）。画面ではこれだけを直す。
      body: string;
      /// 前後の空行（図との区切り）。つなぎ直すときにそのまま戻す。
      lead: string;
      trail: string;
      /// 元の画面で空行だけだった段（図と文章の間の区切り）。画面には出さない。
      spacer: boolean;
    }
  | {
      id: string;
      type: "app";
      figure: AppFigure;
      /// 開きと閉じのフェンスの行（書き直しても残す）。
      open: string;
      close: string;
      /// 組み立てで直したか（直していなければ元の文字列のまま戻す）。
      touched: boolean;
      raw: string;
    }
  | {
      id: string;
      type: "grid";
      /// 表に出すビューの別名（中身の 1 行目・前後の空白は除く）。
      alias: string;
      /// 開きと閉じのフェンスの行（書き直しても残す）。
      open: string;
      close: string;
      /// 組み立てで別名を変えたか（変えていなければ元の文字列のまま戻す）。
      touched: boolean;
      raw: string;
    }
  | { id: string; type: "raw"; raw: string; source: string; reason: RawReason }
) & { pos?: number };

export type RawReason =
  | { kind: "figure" }
  | { kind: "unclosed" }
  | { kind: "diagnostics"; diagnostics: Diagnostic[] }
  | { kind: "round_trip" };

let seq = 0;
const nextId = () => `b${++seq}`;

export function splitScreen(screen: string): ScreenBlock[] {
  // 文章の段は、その中の表（```data_grid）でさらに分ける。つなぎ直しの隣り合いは分けた後の並びで数える。
  const out: ScreenBlock[] = [];
  for (const seg of splitMarkdown(screen)) {
    const parts = seg.type === "markdown" ? splitGrids(seg.text) : [block(seg)];
    for (const b of parts) out.push({ ...b, pos: out.length });
  }
  return out;
}

/// 画面の表（```data_grid）が出しているビューの別名（並びの順・重なりあり）。組み立てと同じ決まりで読む。
export function dataGridAliases(screen: string): string[] {
  return splitScreen(screen).flatMap((b) => (b.type === "grid" ? [b.alias] : []));
}

/// 画面の最後に、ビューを出す表を足す（ビューを束ねたときに、画面で使える形にする・GAP-11）。
export function appendDataGrid(screen: string, alias: string): string {
  const base = screen.replace(/\n+$/, "");
  return `${base ? `${base}\n\n` : ""}\`\`\`data_grid\n${alias}\n\`\`\`\n`;
}

/// コードブロックの開き（行頭の空白 0〜3・``` か ~~~ を 3 文字以上・CommonMark）。表の段にするのは字下げの無いものだけ。
const FENCE = /^( {0,3})(`{3,}|~{3,})(.*)$/;
const GRID_INFO = /^[ \t]*data[_-]grid(?:[\s{]|$)/i;

/// 文章（```ui を含まない Markdown）を、行頭の閉じた ```data_grid で文章と表の段に分ける。1 行も無い段は作らない。
function splitGrids(text: string): ScreenBlock[] {
  const lines = text.split("\n");
  const out: ScreenBlock[] = [];
  let start = 0;
  let i = 0;
  const flush = (end: number) => {
    if (end > start) out.push(textBlock(lines.slice(start, end).join("\n")));
  };
  while (i < lines.length) {
    const open = FENCE.exec(lines[i]!);
    if (!open || (open[2]![0] === "`" && open[3]!.includes("`"))) {
      i++;
      continue;
    }
    const ch = open[2]![0]!;
    const len = open[2]!.length;
    const close = new RegExp(`^ {0,3}${ch === "`" ? "`" : "~"}{${len},}\\s*$`);
    let j = i + 1;
    while (j < lines.length && !close.test(lines[j]!)) j++;
    if (j < lines.length && open[1] === "" && GRID_INFO.test(open[3]!)) {
      flush(i);
      const raw = lines.slice(i, j + 1).join("\n");
      out.push({
        id: nextId(),
        type: "grid",
        alias: (lines[i + 1] !== undefined && i + 1 < j ? lines[i + 1]! : "").trim(),
        open: lines[i]!,
        close: lines[j]!,
        touched: false,
        raw,
      });
      start = j + 1;
    }
    // ほかのコードブロック（閉じていないものは最後まで）の中は読まない。
    i = j + 1;
  }
  flush(lines.length);
  return out;
}

function textBlock(text: string): ScreenBlock {
  const lead = /^\n*/.exec(text)![0];
  const rest = text.slice(lead.length);
  const trail = /\n*$/.exec(rest)![0];
  const body = rest.slice(0, rest.length - trail.length);
  return { id: nextId(), type: "text", body, lead, trail, spacer: !body.trim() };
}

function block(seg: ReturnType<typeof splitMarkdown>[number]): ScreenBlock {
  if (seg.type === "markdown") return textBlock(seg.text);
  const lines = seg.raw.split("\n");
  const isApp = seg.kind === "app" || /^\s*app(\s|$)/i.test(seg.source);
  if (!isApp) return { id: nextId(), type: "raw", raw: seg.raw, source: seg.source, reason: { kind: "figure" } };
  if (!seg.closed) return { id: nextId(), type: "raw", raw: seg.raw, source: seg.source, reason: { kind: "unclosed" } };
  const r = checkEditable(seg.source, seg.kind === "app" ? "app" : undefined);
  if (!r.ok) {
    const reason: RawReason = r.reason === "round_trip" ? { kind: "round_trip" } : { kind: "diagnostics", diagnostics: r.diagnostics };
    return { id: nextId(), type: "raw", raw: seg.raw, source: seg.source, reason };
  }
  return { id: nextId(), type: "app", figure: r.figure, open: lines[0]!, close: lines[lines.length - 1]!, touched: false, raw: seg.raw };
}

function rawOf(b: ScreenBlock): string {
  if (b.type === "text") return `${b.lead}${b.body}${b.trail}`;
  if (b.type === "grid" && b.touched) return `${b.open}\n${b.alias}\n${b.close}`;
  if (b.type === "app" && b.touched) {
    // 箇条書きの中のブロックは字下げしてあるので、中身もフェンスと同じだけ字下げする（リストが途中で切れないように）
    const indent = /^ */.exec(b.open)![0];
    const body = toSource(b.figure)
      .split("\n")
      .map((l) => (l ? `${indent}${l}` : l))
      .join("\n");
    return `${b.open}\n${body}${b.close}`;
  }
  return b.raw;
}

export function joinScreen(blocks: readonly ScreenBlock[]): string {
  let out = "";
  blocks.forEach((b, i) => {
    const raw = rawOf(b);
    if (i > 0) {
      const prev = blocks[i - 1]!;
      // 元の画面で隣り合っていた段は、そのつなぎ目のまま戻す。新しく隣り合った段は空行で区切る
      // （図の前後でも、文章どうしでも。区切らないと段落がつながってしまう）
      const adjacent = prev.pos !== undefined && b.pos !== undefined && prev.pos + 1 === b.pos;
      const needBlank = !adjacent && !out.endsWith("\n") && !raw.startsWith("\n");
      out += needBlank ? "\n\n" : "\n";
    }
    out += raw;
  });
  return out;
}

export function newTextBlock(body = ""): ScreenBlock {
  return { id: nextId(), type: "text", body, lead: "", trail: "", spacer: false };
}

export function newGridBlock(alias: string): ScreenBlock {
  return { id: nextId(), type: "grid", alias, open: "```data_grid", close: "```", touched: true, raw: "" };
}

export function newAppBlock(figure: AppFigure): ScreenBlock {
  return { id: nextId(), type: "app", figure, open: "```ui", close: "```", touched: true, raw: "" };
}
