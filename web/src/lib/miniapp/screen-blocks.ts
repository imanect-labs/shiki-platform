/// ミニアプリの画面（Yui の ```ui を含む Markdown）を、組み立て用の「段」の並びに分けて、つなぎ直す。
///
/// - 文章の段・app の段（組み立てで直せる）・そのままの段（ほかの図・直せない app）の 3 種類。
/// - 何も変えなければ、つなぎ直した文字列は元と 1 文字も変わらない（`splitMarkdown` の行の区切りを
///   そのまま戻すため）。並べ替え・追加で段が隣り合ったときだけ、図の前後に空行を補う。
/// - app の段を直したときは、その段の中身だけを `toSource` の正規形に書き直す。フェンスの行は残す。

import { splitMarkdown, toSource, type AppFigure, type Diagnostic } from "@imanect/yui";
import { checkEditable } from "@imanect/yui/editor";

/// `pos` は元の画面での並び（隣り合っていた段はつなぎ目をそのまま戻す）。新しい段には無い。
export type ScreenBlock = (
  | { id: string; type: "text"; text: string }
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
  if (seg.type === "markdown") return { id: nextId(), type: "text", text: seg.text };
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
  if (b.type === "text") return b.text;
  if (b.type === "app" && b.touched) return `${b.open}\n${toSource(b.figure)}${b.close}`;
  return b.raw;
}

export function joinScreen(blocks: readonly ScreenBlock[]): string {
  let out = "";
  blocks.forEach((b, i) => {
    const raw = rawOf(b);
    if (i > 0) {
      const prev = blocks[i - 1]!;
      // 元の画面で隣り合っていた段は、そのつなぎ目のまま戻す。新しく隣り合った図の前後は空行で区切る
      const adjacent = prev.pos !== undefined && b.pos !== undefined && prev.pos + 1 === b.pos;
      const needBlank = !adjacent && (prev.type !== "text" || b.type !== "text") && !(prev.type === "text" && prev.text.endsWith("\n")) && !(b.type === "text" && b.text.startsWith("\n"));
      out += needBlank ? "\n\n" : "\n";
    }
    out += raw;
  });
  return out;
}

export function newTextBlock(text = ""): ScreenBlock {
  return { id: nextId(), type: "text", text };
}

export function newAppBlock(figure: AppFigure): ScreenBlock {
  return { id: nextId(), type: "app", figure, open: "```ui", close: "```", touched: true, raw: "" };
}
