/// 承認カードの中身（どのファイルに・何をして・何が起きるか）を、承認が要るツールの入力から組み立てる（#1339）。
///
/// 生のツール名・共通の定型文・入力の JSON だけでは、承認する人は何を許すのかが読めない。ここでツールごとに
/// 「対象」「すること（操作ごとに 1 行）」「起きること（副作用・取り消せるか）」へ写す。
///
/// 起きることの文言は各ツールの実際の振る舞い（`crates/chat/src/*_tool.rs`・`crates/agent-core/src/tools/` の
/// description と実装）に合わせる。振る舞いを変えたらここも直す。読めない入力でもカードは壊さず、分かる範囲だけ
/// 出す（実行してよいかは承認の後にツールとストレージが改めて確かめる。ここは表示だけ）。
///
/// 業務データの書込（`data_record_write`）とファイルの移動（`drive.move`）は専用の中身（`data-approval-summary.tsx`・
/// `drive-move-approval-summary.tsx`）があるので、ここでは扱わない。

import type { ToolName } from "@/generated/gui-spec";

/// 「すること」の 1 行。`destructive` は内容を消す操作（目立たせる）。
export type ApprovalStep = { text: string; destructive?: boolean };

/// 「起きること」の 1 行。`warn` は消える・上書きされる・取り消しにくいもの。
export type ApprovalEffect = { text: string; tone: "info" | "warn" };

/// 保存先・複製先のフォルダ。`id` が null のときは `fallback` をそのまま出す（省略時の既定）。
export type ApprovalFolder = { label: string; id: string | null; fallback: string };

export type ApprovalDetail = {
  /// 対象のファイルの node_id（名前と場所を本人の権限で引く）。
  nodeId: string | null;
  /// 保存先・複製先のフォルダ。
  folder: ApprovalFolder | null;
  /// モデルが書いた変更の要約（`office.excel_edit` / `office.word_edit` の `summary`）。
  summary: string | null;
  /// すること（上限まで）。
  steps: ApprovalStep[];
  /// 上限を超えて出さなかった操作の数。
  more: number;
  /// コマンド・書き込む内容の抜粋（等幅で出す）。
  code: { label: string; text: string } | null;
  /// 起きること。
  effects: ApprovalEffect[];
};

/// 「すること」に出す操作の上限（超えた分は「ほか N 件」）。
const STEP_LIMIT = 6;
/// 操作の中の語句 1 つを出す長さの上限。
const PHRASE_CHARS = 32;
/// コマンド・内容の抜粋の長さの上限。
const CODE_CHARS = 400;

export const ROOT_FOLDER_LABEL = "マイドライブ（ルート）";

type Input = Record<string, unknown>;

function rec(v: unknown): Input {
  return v && typeof v === "object" && !Array.isArray(v) ? (v as Input) : {};
}

function str(o: Input, key: string): string | null {
  const v = o[key];
  if (typeof v !== "string") return null;
  const t = v.trim();
  return t.length > 0 ? t : null;
}

function num(o: Input, key: string): number | null {
  const v = o[key];
  return typeof v === "number" && Number.isFinite(v) ? v : null;
}

/// 語句を 1 行に畳み、長ければ末尾を「…」にして鉤括弧で包む。
function q(s: string | null, max = PHRASE_CHARS): string {
  if (s === null) return "「（指定なし）」";
  const t = s.replace(/\s+/g, " ").trim();
  return `「${t.length <= max ? t : `${t.slice(0, max)}…`}」`;
}

/// HTML の本文から文字だけを取り出す（表示のための抜粋。サニタイズはサーバが行う）。
function textOfHtml(html: string | null): string | null {
  if (html === null) return null;
  const t = html.replace(/<[^>]*>/g, " ").replace(/&nbsp;/g, " ").replace(/\s+/g, " ").trim();
  return t.length > 0 ? t : null;
}

function clipCode(s: string): string {
  return s.length <= CODE_CHARS ? s : `${s.slice(0, CODE_CHARS)}\n…（ほか ${(s.length - CODE_CHARS).toLocaleString()} 文字）`;
}

function opsOf(o: Input): Input[] {
  const ops = o.ops;
  return Array.isArray(ops) ? ops.map(rec) : [];
}

/// 操作ごとに 1 行へ写し、上限で切る。
function stepsOf(ops: Input[], one: (op: Input) => ApprovalStep): { steps: ApprovalStep[]; more: number } {
  const all = ops.map(one);
  return { steps: all.slice(0, STEP_LIMIT), more: Math.max(0, all.length - STEP_LIMIT) };
}

/// 消す操作が入っていれば、起きることの先頭に注意を足す（上限で畳んだ分も数える）。
function destructiveEffect(ops: Input[], one: (op: Input) => ApprovalStep): ApprovalEffect[] {
  const n = ops.map(one).filter((s) => s.destructive).length;
  return n > 0 ? [{ tone: "warn", text: `内容を消す操作が ${n} 件あります（することの赤字）。` }] : [];
}

function cellCount(rows: unknown): { rows: number; cols: number } | null {
  if (!Array.isArray(rows)) return null;
  const cols = rows.reduce<number>((m, r) => (Array.isArray(r) ? Math.max(m, r.length) : m), 0);
  return { rows: rows.length, cols };
}

const SHARED_SEE = "このファイルを共有している人にも、変更後の内容が見えます。";
const FOLDER_SEE = "保存先のフォルダを共有している人からも見えます。";

// ── ノート（document.edit）────────────────────────────────────

function noteStep(op: Input): ApprovalStep {
  const find = str(op, "find");
  const heading = str(op, "heading");
  const within = heading ? `見出し${q(heading)}の節で、` : "";
  switch (str(op, "op")) {
    case "replace_text": {
      const rep = typeof op.replace === "string" ? op.replace : "";
      return rep.trim() === ""
        ? { text: `${within}${q(find)}を削除（段落・項目ごと消える）`, destructive: true }
        : { text: `${within}${q(find)}を${q(rep)}に置き換え` };
    }
    case "replace_block":
      return { text: `${within}${q(find)}を含む段落・項目を書き直し` };
    case "insert_after":
      return { text: `${within}${q(find)}の後ろに追加` };
    case "replace_section":
      return { text: `見出し${q(heading)}の節の本文を置き換え`, destructive: true };
    case "insert_after_heading":
      return { text: `見出し${q(heading)}の後ろに追加` };
    case "delete_section":
      return { text: `見出し${q(heading)}の節を見出しごと削除`, destructive: true };
    case "move_section": {
      const before = str(op, "before_heading");
      return { text: `見出し${q(heading)}の節を${before ? `${q(before)}の前へ` : "末尾へ"}移動` };
    }
    case "append":
      return { text: "末尾に追記" };
    case "replace_all":
      return { text: "全文を書き直し（今の本文は置き換わる）", destructive: true };
    case "set_meta":
      return { text: `プロパティ${q(str(op, "key"))}を${q(str(op, "value"))}に設定` };
    default:
      return { text: `その他の変更（${str(op, "op") ?? "不明"}）` };
  }
}

function documentEdit(o: Input): ApprovalDetail {
  const ops = opsOf(o);
  const suggest = o.mode === "suggest";
  return {
    nodeId: str(o, "node_id"),
    folder: null,
    summary: null,
    ...stepsOf(ops, noteStep),
    code: null,
    effects: [
      ...destructiveEffect(ops, noteStep),
      suggest
        ? { tone: "info", text: "提案として入ります。ノートを開いた人が採用するまで、本文は変わりません。" }
        : { tone: "info", text: "ノートの本文に直接反映され、開いている人の画面にもすぐ出ます。" },
      { tone: "info", text: "操作はすべて適用されるか、1 つも適用されないかのどちらかです。" },
      ...(suggest ? [] : [{ tone: "info" as const, text: SHARED_SEE }]),
    ],
  };
}

// ── スライド（slide.edit）──────────────────────────────────────

function slideStep(op: Input): ApprovalStep {
  switch (str(op, "op")) {
    case "append_slide":
      return { text: "末尾にスライドを 1 枚追加" };
    case "insert_slide_after":
      return { text: "指定したスライドの後ろに 1 枚追加" };
    case "replace_slide": {
      const t = textOfHtml(str(op, "html"));
      return { text: t ? `スライド 1 枚の本文を${q(t)}に書き直し` : "スライド 1 枚の本文を書き直し", destructive: true };
    }
    case "remove_slide":
      return { text: "スライドを 1 枚削除", destructive: true };
    case "set_notes":
      return { text: "スピーカーノートを書き換え" };
    case "set_background":
      return { text: "背景を変更" };
    case "set_meta":
      return { text: `プロパティ${q(str(op, "key"))}を${q(str(op, "value"))}に設定` };
    default:
      return { text: `その他の変更（${str(op, "op") ?? "不明"}）` };
  }
}

function slideEdit(o: Input): ApprovalDetail {
  const ops = opsOf(o);
  return {
    nodeId: str(o, "node_id"),
    folder: null,
    summary: null,
    ...stepsOf(ops, slideStep),
    code: null,
    effects: [
      ...destructiveEffect(ops, slideStep),
      { tone: "info", text: "スライドに直接反映され、開いている人の画面にもすぐ出ます（同時に編集していても壊れません）。" },
      { tone: "info", text: SHARED_SEE },
    ],
  };
}

// ── Office（office.edit・office.live_edit）──────────────────────

function officeStep(op: Input): ApprovalStep {
  const sheet = str(op, "sheet");
  const onSheet = sheet ? `シート${q(sheet)}の` : "";
  switch (str(op, "op")) {
    case "replace_text":
      return { text: `${q(str(op, "find"))}を${q(str(op, "replace"))}に置き換え` };
    case "append_markdown":
      return { text: "末尾に追記" };
    case "insert_after_heading":
      return { text: `見出し${q(str(op, "heading"))}の後ろに追加` };
    case "set_cells": {
      const cells = Object.keys(rec(op.cells));
      const shown = cells.slice(0, 4).join("・");
      return {
        text: cells.length > 0
          ? `${onSheet}セル ${shown}${cells.length > 4 ? ` ほか ${cells.length - 4} か所` : ""} を書き換え`
          : `${onSheet}セルを書き換え`,
      };
    }
    case "insert_rows": {
      const n = Array.isArray(op.rows) ? op.rows.length : null;
      return { text: `${onSheet}${n !== null ? `${n} 行を` : "行を"}挿入` };
    }
    case "add_sheet":
      return { text: `シート${q(str(op, "name"))}を追加` };
    case "add_slide":
      return { text: `スライド${q(str(op, "title"))}を追加` };
    case "remove_slide":
      return { text: "スライドを 1 枚削除", destructive: true };
    default:
      return { text: `その他の変更（${str(op, "op") ?? "不明"}）` };
  }
}

function officeEdit(o: Input): ApprovalDetail {
  const ops = opsOf(o);
  return {
    nodeId: str(o, "node_id"),
    folder: null,
    summary: null,
    ...stepsOf(ops, officeStep),
    code: null,
    effects: [
      ...destructiveEffect(ops, officeStep),
      { tone: "info", text: "新しい版として保存されます（前の版は版の履歴に残ります）。" },
      {
        tone: "info",
        text: "誰かが編集画面で開いている間は上書きせず、提案の版として保存されます（版の履歴から採用すると反映されます）。",
      },
      { tone: "info", text: SHARED_SEE },
    ],
  };
}

function liveStep(op: Input): ApprovalStep {
  switch (str(op, "op")) {
    case "replace_text": {
      const t = textOfHtml(str(op, "html"));
      return { text: `${q(str(op, "find"))}を${t ? q(t) : "新しい内容"}に置き換え` };
    }
    case "append_html":
      return { text: "末尾に追記" };
    case "set_cells": {
      const size = cellCount(op.rows);
      const anchor = str(op, "anchor");
      return {
        text: `${anchor ? `セル ${anchor} から` : ""}${size ? `${size.rows} 行 × ${size.cols} 列の` : ""}値を貼り込み（そこにある値は上書き）`,
      };
    }
    default:
      return { text: `その他の変更（${str(op, "op") ?? "不明"}）` };
  }
}

function officeLiveEdit(o: Input): ApprovalDetail {
  const ops = opsOf(o);
  return {
    nodeId: str(o, "node_id"),
    folder: null,
    summary: null,
    ...stepsOf(ops, liveStep),
    code: null,
    effects: [
      { tone: "info", text: "開いている人の編集画面にすぐ反映され、新しい版として保存されます。" },
      { tone: "info", text: SHARED_SEE },
    ],
  };
}

// ── Office Engine（office.excel_edit・office.word_edit）─────────

/// 範囲の書き方（"B2:D10"・配列）を 1 つの文字列にする。
function rangeOf(op: Input, key = "range"): string | null {
  const v = op[key];
  if (typeof v === "string" && v.trim()) return v.trim();
  if (Array.isArray(v)) {
    const parts = v.filter((x): x is string => typeof x === "string" && x.trim() !== "");
    return parts.length > 0 ? parts.join("・") : null;
  }
  return null;
}

function at(r: string | null): string {
  return r ? `${r} ` : "";
}

function excelStep(op: Input): ApprovalStep {
  const r = rangeOf(op);
  const sheet = str(op, "sheet");
  switch (str(op, "op")) {
    case "set_values":
      return { text: `${at(r)}に値・数式を入力（そこにある値は上書き）` };
    case "fill_formula":
      return { text: `${at(r)}に数式 ${str(op, "formula") ?? ""} を展開`.replace(/ +$/, "") };
    case "format":
      return { text: `${at(r)}の書式を変更` };
    case "style_table":
      return { text: `${at(r)}を表の体裁に整える` };
    case "merge":
      return { text: `${at(r)}のセルを結合` };
    case "unmerge":
      return { text: `${at(r)}のセルの結合を解除` };
    case "column_width":
      return { text: `列 ${str(op, "columns") ?? ""} の幅を変更` };
    case "row_height":
      return { text: `行 ${str(op, "rows") ?? ""} の高さを変更` };
    case "insert_rows":
      return { text: `${num(op, "before") ?? "?"} 行目の前に ${num(op, "count") ?? 1} 行を挿入` };
    case "delete_rows":
      return { text: `行 ${rangeOf(op, "rows") ?? ""} を削除`, destructive: true };
    case "insert_columns":
      return { text: `${str(op, "before") ?? "?"} 列の前に ${num(op, "count") ?? 1} 列を挿入` };
    case "delete_columns":
      return { text: `列 ${rangeOf(op, "columns") ?? ""} を削除`, destructive: true };
    case "hide_rows":
    case "show_rows":
      return { text: `行 ${rangeOf(op, "rows") ?? ""} を${op.op === "hide_rows" ? "隠す" : "再表示"}` };
    case "hide_columns":
    case "show_columns":
      return { text: `列 ${rangeOf(op, "columns") ?? ""} を${op.op === "hide_columns" ? "隠す" : "再表示"}` };
    case "clear": {
      const what = { contents: "内容", formats: "書式", all: "内容と書式" }[str(op, "what") ?? ""] ?? "内容";
      return { text: `${at(r)}の${what}を消去`, destructive: what !== "書式" };
    }
    case "sort":
      return { text: `${at(r)}を並べ替え` };
    case "autofilter":
      return { text: op.enabled === false ? "フィルタを解除" : `${at(r)}にフィルタを付ける` };
    case "freeze":
      return { text: op.cell === null ? "ウィンドウ枠の固定を解除" : `${str(op, "cell") ?? ""} でウィンドウ枠を固定` };
    case "add_sheet":
      return { text: `シート${q(str(op, "name"))}を追加` };
    case "rename_sheet":
      return { text: `シート${q(sheet)}の名前を${q(str(op, "name"))}に変更` };
    case "delete_sheet":
      return { text: `シート${q(sheet)}を削除`, destructive: true };
    case "move_sheet":
      return { text: `シート${q(sheet)}の位置を変更` };
    case "sheet_props":
      return { text: `シート${q(sheet)}の表示・色を変更` };
    case "conditional_format":
      return { text: `${at(r)}に条件付き書式を付ける` };
    case "clear_conditional_formats":
      return { text: `${at(r)}の条件付き書式を外す` };
    case "data_validation":
      return { text: `${at(r)}に入力規則を付ける` };
    case "define_name":
      return op.range === null
        ? { text: `名前${q(str(op, "name"))}を削除` }
        : { text: `名前${q(str(op, "name"))}を定義` };
    case "add_chart":
      return { text: `グラフ${str(op, "title") ? q(str(op, "title")) : ""}を追加` };
    case "delete_chart":
      return { text: `グラフ${q(str(op, "name"))}を削除`, destructive: true };
    case "page_setup":
      return { text: "印刷の設定を変更" };
    default:
      return { text: `その他の変更（${str(op, "op") ?? "不明"}）` };
  }
}

function wordStep(op: Input): ApprovalStep {
  switch (str(op, "op")) {
    case "insert": {
      const t = textOfHtml(str(op, "html"));
      const where = op.at === "start" ? "先頭に" : op.at === "end" ? "末尾に" : "本文に";
      return { text: t ? `${where}${q(t)}を追加` : `${where}追加` };
    }
    case "replace": {
      const t = textOfHtml(str(op, "html"));
      const all = op.target === "all";
      return {
        text: all ? "文書全体を書き直し（今の本文は置き換わる）" : t ? `ブロックを${q(t)}に書き直し` : "ブロックを書き直し",
        destructive: true,
      };
    }
    case "delete":
      return { text: op.target === "all" ? "本文をすべて削除" : "ブロックを削除", destructive: true };
    case "replace_text": {
      const rep = typeof op.replace === "string" ? op.replace : "";
      const all = op.all === true ? "（すべての箇所）" : "";
      return rep.trim() === ""
        ? { text: `${q(str(op, "find"))}を削除${all}`, destructive: true }
        : { text: `${q(str(op, "find"))}を${q(rep)}に置き換え${all}` };
    }
    case "format_text":
      return { text: `${q(str(op, "find"))}の文字の書式を変更` };
    case "format_paragraph":
      return { text: "段落の書式を変更" };
    case "set_style":
      return { text: `スタイル${q(str(op, "name"))}を変更` };
    case "page_setup":
      return { text: "用紙・余白を変更" };
    case "header":
      return { text: op.content === null ? "ヘッダーを消す" : "ヘッダーを設定" };
    case "footer":
      return { text: op.content === null ? "フッターを消す" : "フッターを設定" };
    case "insert_toc":
      return { text: "目次を追加" };
    case "update_toc":
      return { text: "目次を更新" };
    case "insert_rows":
      return { text: `表に ${num(op, "count") ?? 1} 行を挿入` };
    case "delete_rows":
      return { text: `表の行 ${str(op, "rows") ?? ""} を削除`, destructive: true };
    case "set_cells":
      return { text: `表のセル ${str(op, "cell") ?? ""} から文字を書き込み（そこにある文字は上書き）` };
    case "format_cells":
      return { text: "表のセルの書式を変更" };
    case "table_props":
      return { text: "表の幅・配置を変更" };
    case "style_table":
      return { text: "表の体裁を整える" };
    case "insert_chart":
      return { text: `グラフ${str(op, "title") ? q(str(op, "title")) : ""}を追加` };
    case "page_break":
      return { text: "改ページを入れる" };
    case "title_page":
      return { text: "表紙の設定を変更" };
    case "track_changes":
      return { text: op.enabled === false ? "変更履歴の記録を止める" : "変更履歴の記録を始める" };
    case "accept_all_changes":
      return { text: "未承認の変更をすべて確定" };
    case "reject_all_changes":
      return { text: "未承認の変更をすべて取り消し", destructive: true };
    default:
      return { text: `その他の変更（${str(op, "op") ?? "不明"}）` };
  }
}

function engineEdit(o: Input, one: (op: Input) => ApprovalStep): ApprovalDetail {
  const ops = opsOf(o);
  return {
    nodeId: str(o, "node_id"),
    folder: null,
    summary: str(o, "summary"),
    ...stepsOf(ops, one),
    code: null,
    effects: [
      ...destructiveEffect(ops, one),
      { tone: "info", text: "開いている人の画面にすぐ反映され、版として保存されます。" },
      { tone: "info", text: "操作はすべて適用されるか、1 つも適用されないかのどちらかです。" },
      { tone: "info", text: "編集画面の「元に戻す」1 回で取り消せます。" },
      { tone: "info", text: SHARED_SEE },
    ],
  };
}

// ── CSV（csv.patch・csv.write）──────────────────────────────────

function csvStep(op: Input): ApprovalStep {
  const row = num(op, "row");
  const col = num(op, "col");
  // 行・列は 0 始まり（見出しの行を除いたデータ行）。人には 1 始まりで見せる。
  const rowLabel = row !== null ? `${row + 1} 行目` : "行";
  const colLabel = col !== null ? `${col + 1} 列目` : "列";
  switch (str(op, "op")) {
    case "cell_update":
      return { text: `${rowLabel}・${colLabel}のセルを${q(typeof op.value === "string" ? op.value : "")}に変更` };
    case "row_insert": {
      const atRow = num(op, "at");
      return { text: `${atRow !== null ? `${atRow + 1} 行目に` : ""}行を 1 行挿入` };
    }
    case "row_delete":
      return { text: `${rowLabel}を削除`, destructive: true };
    case "column_add":
      return { text: `末尾に列${q(str(op, "name"))}を追加` };
    case "column_delete":
      return { text: `${colLabel}を削除（その列の値がすべて消える）`, destructive: true };
    case "column_rename":
      return { text: `${colLabel}の名前を${q(str(op, "name"))}に変更` };
    default:
      return { text: `その他の変更（${str(op, "op") ?? "不明"}）` };
  }
}

function csvPatch(o: Input): ApprovalDetail {
  const ops = opsOf(o);
  return {
    nodeId: str(o, "node_id"),
    folder: null,
    summary: null,
    ...stepsOf(ops, csvStep),
    code: null,
    effects: [
      ...destructiveEffect(ops, csvStep),
      { tone: "info", text: "新しい版として保存されます（前の版は版の履歴に残ります）。" },
      { tone: "info", text: "AI が読んだ後に誰かが更新していたら、上書きせずに失敗します。" },
      { tone: "info", text: "編集の後、会話に出るファイルのカードから元に戻せます。" },
      { tone: "info", text: SHARED_SEE },
    ],
  };
}

function lineCount(s: string | null): number {
  if (!s) return 0;
  return s.replace(/\r?\n$/, "").split(/\r?\n/).length;
}

function createIn(o: Input, label = "保存先"): ApprovalFolder {
  const parent = str(o, "parent_id");
  return { label, id: parent && parent !== "root" ? parent : null, fallback: ROOT_FOLDER_LABEL };
}

function csvWrite(o: Input): ApprovalDetail {
  const csv = typeof o.csv === "string" ? o.csv : null;
  const lines = lineCount(csv);
  return {
    nodeId: null,
    folder: createIn(o),
    summary: null,
    steps: [{ text: lines > 0 ? `見出しの行と ${Math.max(0, lines - 1)} 行のデータで CSV を作る` : "空の CSV を作る" }],
    more: 0,
    code: null,
    effects: [
      { tone: "info", text: "ドライブに新しいファイルを作ります。今あるファイルは変わりません。" },
      { tone: "info", text: FOLDER_SEE },
    ],
  };
}

function saveDocument(o: Input): ApprovalDetail {
  const md = str(o, "markdown");
  return {
    nodeId: null,
    folder: createIn(o),
    summary: null,
    steps: [{ text: md ? `本文（${md.length.toLocaleString()} 文字）を書いた Word 文書を作る` : "空の Word 文書を作る" }],
    more: 0,
    code: md ? { label: "本文の冒頭", text: clipCode(md) } : null,
    effects: [
      { tone: "info", text: "ドライブに新しいファイルを作ります。今あるファイルは変わりません。" },
      { tone: "info", text: FOLDER_SEE },
    ],
  };
}

function saveSheet(o: Input): ApprovalDetail {
  const size = cellCount(o.rows);
  return {
    nodeId: null,
    folder: createIn(o),
    summary: null,
    steps: [
      { text: size && size.rows > 0 ? `A1 から ${size.rows} 行 × ${size.cols} 列の値を入れた Excel ブックを作る` : "空の Excel ブックを作る" },
    ],
    more: 0,
    code: null,
    effects: [
      { tone: "info", text: "ドライブに新しいファイルを作ります。今あるファイルは変わりません。" },
      { tone: "info", text: FOLDER_SEE },
    ],
  };
}

// ── ドライブの複製（drive.copy）─────────────────────────────────

function driveCopy(o: Input): ApprovalDetail {
  const dest = str(o, "destination_folder_id");
  const newName = str(o, "new_name");
  return {
    nodeId: str(o, "node_id"),
    folder: {
      label: "複製先",
      id: dest && dest !== "root" ? dest : null,
      fallback: dest === "root" ? ROOT_FOLDER_LABEL : "元と同じフォルダ",
    },
    summary: null,
    steps: [{ text: newName ? `${q(newName, 60)}という名前で複製を作る` : "複製を作る" }],
    more: 0,
    code: null,
    effects: [
      { tone: "info", text: "元のファイルは変わりません。同じ内容の新しいファイルができます。" },
      { tone: "info", text: "複製先のフォルダを共有している人からも見えます。" },
    ],
  };
}

// ── 自律エージェントの作業（shell・fs_*）────────────────────────

function shell(o: Input): ApprovalDetail {
  const cmd = typeof o.cmd === "string" ? o.cmd : "";
  return {
    nodeId: null,
    folder: null,
    summary: null,
    steps: [],
    more: 0,
    code: { label: "実行するコマンド", text: clipCode(cmd) },
    effects: [
      { tone: "info", text: "外から隔てられた実行環境でコマンドを動かします。ネットワークにはつながりません。" },
      { tone: "warn", text: "AI の作業フォルダのファイルを書き換えたり、消したりすることがあります。" },
    ],
  };
}

function fsContent(o: Input, label: string): ApprovalDetail["code"] {
  const c = typeof o.content === "string" ? o.content : null;
  return c ? { label: `${label}（${c.length.toLocaleString()} 文字）`, text: clipCode(c) } : null;
}

function fsWrite(o: Input): ApprovalDetail {
  return {
    nodeId: null,
    folder: null,
    summary: null,
    steps: [],
    more: 0,
    code: fsContent(o, "書き込む内容"),
    effects: [
      { tone: "warn", text: "同じ名前のファイルがあれば、中身をまるごと置き換えます（前の内容は版の履歴に残ります）。" },
      { tone: "info", text: "AI の作業フォルダの中のファイルです。" },
    ],
  };
}

function fsAppend(o: Input): ApprovalDetail {
  return {
    nodeId: null,
    folder: null,
    summary: null,
    steps: [],
    more: 0,
    code: fsContent(o, "末尾に足す内容"),
    effects: [
      { tone: "info", text: "ファイルの末尾に足します（無ければ作ります）。今ある内容は消えません。" },
      { tone: "info", text: "AI の作業フォルダの中のファイルです。" },
    ],
  };
}

function fsEdit(o: Input): ApprovalDetail {
  const from = typeof o.old_string === "string" ? o.old_string : null;
  const to = typeof o.new_string === "string" ? o.new_string : null;
  return {
    nodeId: null,
    folder: null,
    summary: null,
    steps: [{ text: `${q(from)}を${q(to)}に置き換え`, destructive: to !== null && to.trim() === "" }],
    more: 0,
    code: null,
    effects: [
      { tone: "info", text: "置き換えた内容を新しい版として保存します（前の版は残ります）。" },
      { tone: "info", text: "AI の作業フォルダの中のファイルです。" },
    ],
  };
}

function fsDelete(): ApprovalDetail {
  return {
    nodeId: null,
    folder: null,
    summary: null,
    steps: [],
    more: 0,
    code: null,
    effects: [
      { tone: "warn", text: "ファイルをゴミ箱へ移します（ゴミ箱から戻せます）。検索の対象からも外れます。" },
      { tone: "info", text: "AI の作業フォルダの中のファイルです。" },
    ],
  };
}

/// 承認が要るツールごとの中身の組み立て（名前は生成した語彙で縛る）。
const DETAIL: Partial<Record<ToolName, (input: Input) => ApprovalDetail>> = {
  "document.edit": documentEdit,
  "slide.edit": slideEdit,
  "office.edit": officeEdit,
  "office.live_edit": officeLiveEdit,
  "office.excel_edit": (o) => engineEdit(o, excelStep),
  "office.word_edit": (o) => engineEdit(o, wordStep),
  "csv.patch": csvPatch,
  "csv.write": csvWrite,
  save_document: saveDocument,
  save_sheet: saveSheet,
  "drive.copy": driveCopy,
  shell,
  fs_write: fsWrite,
  fs_append: fsAppend,
  fs_edit: fsEdit,
  fs_delete: fsDelete,
};

/// 承認カードの中身。ここに無いツールは null（呼び出し側は理由と送る内容を出す）。
export function approvalDetail(name: string, input: unknown): ApprovalDetail | null {
  const build = DETAIL[name as ToolName];
  return build ? build(rec(input)) : null;
}
