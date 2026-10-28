/// ミニアプリの画面（Yui の ```ui を含む Markdown）を、組み立て用の「段」の並びに分けて、つなぎ直す。
///
/// - 文章の段・app の段（組み立てで直せる）・そのままの段（ほかの図・直せない app）の 3 種類。
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
  return splitMarkdown(screen).map((seg, pos): ScreenBlock => ({ ...block(seg), pos }));
}

function block(seg: ReturnType<typeof splitMarkdown>[number]): ScreenBlock {
  if (seg.type === "markdown") {
    const lead = /^\n*/.exec(seg.text)![0];
    const rest = seg.text.slice(lead.length);
    const trail = /\n*$/.exec(rest)![0];
    const body = rest.slice(0, rest.length - trail.length);
    return { id: nextId(), type: "text", body, lead, trail, spacer: !body.trim() };
  }
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

export function newAppBlock(figure: AppFigure): ScreenBlock {
  return { id: nextId(), type: "app", figure, open: "```ui", close: "```", touched: true, raw: "" };
}
