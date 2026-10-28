/// 値の入力欄（value-expr-input.tsx）の参照・差し込みの計算（描画を持たない・BUG-1）。
///
/// - 参照の候補: 入力項目（input_schema の properties）・祖先ブロックの出力（設定で決まる項目を含む:
///   「AI に聞く」の出力の形の `data/<項目>`・「AI で判定」の質問ごとの答え）・map 領域内の each・
///   きっかけの内容の項目（イベントのカタログの単一定義・D-47）・入力全体・きっかけの内容全体。
///   参照先の既定を「入力全体」にせず、ここから選ばせる。
/// - 差し込み名の自動命名（path の末尾。同じ名前で別の参照があれば連番）。
/// - テンプレートの字句（`{name}` が差し込み・`{{` `}}` は波括弧そのもの・ir.md §3.2）。規則は実行時の
///   crates/workflow-engine/src/ir/expr.rs の `template_pieces` と同じ（保存時の検証もそれで読む）。
/// - 固定値に書かれた差し込みのつもりの文字（`{{x}}`・`{x}`・`${x}`）の検出と、テンプレートへの変換。
/// - 入力項目への参照（`$from: "input"` の path の先頭の段）の収集と、キーの変更への追随（GAP-12）。

import { EVENT_CATALOG, NODE_CATALOG, RUN_INFO_CATALOG } from "@/generated/workflow-catalog";
import type { FromRef, TemplateExpr, ValueExpr } from "@/generated/workflow-ir";
import type { InputFieldInfo } from "@/lib/workflow-input-schema";
import { outputSchemaKeys } from "@/lib/workflow-output-schema";

/// `nodes.<id>.output` の参照候補（祖先ブロック）。`type` があれば出力の形から項目の候補も出す。
/// `params` はそのブロックの設定（設定で決まる出力の項目の候補を出すのに使う）。
export type RefCandidate = { id: string; label: string; type?: string; params?: unknown };

/// 組み立てる参照（`path`・`default` は省略できる）。
export type RefValue = Pick<FromRef, "$from"> & Partial<Pick<FromRef, "path" | "default">>;

/// 候補の 1 つ。`key` は `$from` と `path` から作る（同じ参照は同じ key）。
export type RefOption = {
  key: string;
  ref: RefValue;
  label: string;
  group: string;
};

export type RefContext = {
  inputFields: InputFieldInfo[];
  ancestors: RefCandidate[];
  inMapRegion: boolean;
  /// ワークフローのイベントトリガの source（きっかけの内容の項目を候補に出す・BUG-36）。
  triggerSources?: string[];
};

export const SOURCE_LABELS: Record<string, string> = {
  input: "この実行の入力",
  trigger: "きっかけの内容",
  each: "繰り返し中の要素",
  run: "実行情報",
};

/// 実行情報の候補の見出し（日時は日本時間で入る・vocab の run_info。実行した人も並ぶので、日本時間が日時の
/// ことだと分かるように言う・D-101）。
const RUN_INFO_GROUP = `${SOURCE_LABELS.run}（日時は日本時間）`;

export function refKey(ref: { $from: string; path?: string | null }): string {
  return `${ref.$from}|${ref.path ?? ""}`;
}

export function isFromRef(v: unknown): v is RefValue {
  return v !== null && typeof v === "object" && typeof (v as { $from?: unknown }).$from === "string";
}

/// JSON Pointer の 1 段（`~` と `/` をエスケープする）。
const pointerSegment = (s: string) => s.replace(/~/g, "~0").replace(/\//g, "~1");
const unpointerSegment = (s: string) => s.replace(/~1/g, "/").replace(/~0/g, "~");

function pathOf(segments: string[]): string | undefined {
  return segments.length > 0 ? `/${segments.map(pointerSegment).join("/")}` : undefined;
}

/// ノード種の出力の最上位の項目名（カタログの `output_fields`・D-42）。項目の組でない出力は空。
///
/// 項目はカタログの単一定義から生成される（crates/workflow-engine/src/vocab/catalog.rs）。表記の文字列
/// （`output_shape`）は人と AI が読むためのもので、ここでは字句解析しない。
export function outputKeys(nodeType: string | undefined): string[] {
  const entry = NODE_CATALOG.find((c) => c.type === nodeType);
  return (entry?.output_fields ?? []).map((f) => f.name);
}

/// 祖先ブロックの出力の項目の候補（path の段と表示）。最上位はカタログの `output_fields`（[`outputKeys`]）で、
/// 設定で決まる項目をその後ろに足す:
/// - 「AI に聞く」（llm.invoke）: `data` は出力の形（`output_schema`）を書いたときだけ出し、その項目ごとに
///   `data/<項目>` を足す（ir.md §7.3）。
/// - 「AI で判定」（ai.decide）: 質問ごとに答え（`answers/<キー>/value`）と確からしさ（`/confidence`）、
///   段階の質問は段階の番号（`/level_index`・0 から）を足す（ir.md §7.11）。
export function outputRefFields(a: RefCandidate): { path: string[]; label: string }[] {
  const params = (a.params ?? {}) as Record<string, unknown>;
  const out: { path: string[]; label: string }[] = [];
  for (const k of outputKeys(a.type)) {
    if (a.type === "llm.invoke" && k === "data") {
      if (params.output_schema === undefined || params.output_schema === null) continue;
      out.push({ path: [k], label: k });
      for (const key of outputSchemaKeys(params.output_schema)) out.push({ path: [k, key], label: `${k}.${key}` });
      continue;
    }
    out.push({ path: [k], label: k });
    if (a.type === "ai.decide" && k === "answers" && Array.isArray(params.questions)) {
      for (const q of params.questions as { key?: unknown; type?: unknown }[]) {
        if (typeof q?.key !== "string" || q.key === "") continue;
        out.push({ path: [k, q.key, "value"], label: `${q.key} の答え` });
        out.push({ path: [k, q.key, "confidence"], label: `${q.key} の確からしさ` });
        if (q.type === "score") out.push({ path: [k, q.key, "level_index"], label: `${q.key} の段階の番号` });
      }
    }
  }
  return out;
}

/// きっかけの内容の項目（イベントトリガの source ごとのペイロードの項目・カタログの単一定義・D-47）。
/// 複数のきっかけが同じ項目を持てば 1 つにまとめる。
export function triggerFields(sources: string[] | undefined): { name: string; description: string }[] {
  const out: { name: string; description: string }[] = [];
  for (const source of sources ?? []) {
    const entry = EVENT_CATALOG.find((e) => e.source === source);
    for (const f of entry?.payload_fields ?? []) {
      if (!out.some((o) => o.name === f.name)) out.push({ name: f.name, description: f.description_ja });
    }
  }
  return out;
}

/// 参照の候補（差し込むメニューと参照先の選択に使う）。並びは画面の並び。
export function refOptions({ inputFields, ancestors, inMapRegion, triggerSources }: RefContext): RefOption[] {
  const out: RefOption[] = [];
  const add = (ref: RefValue, label: string, group: string) => out.push({ key: refKey(ref), ref, label, group });
  for (const f of inputFields) {
    add({ $from: "input", path: pathOf([f.key]) }, f.title ? `${f.title}（${f.key}）` : f.key, "入力項目");
  }
  for (const a of ancestors) {
    const from = `nodes.${a.id}.output`;
    for (const f of outputRefFields(a)) {
      add({ $from: from, path: pathOf(f.path) }, `「${a.label}」の ${f.label}`, "前のブロックの結果");
    }
    add({ $from: from }, `「${a.label}」の結果（全体）`, "前のブロックの結果");
  }
  if (inMapRegion) {
    add({ $from: "each", path: "/item" }, "繰り返し中の要素", "繰り返し");
    add({ $from: "each", path: "/index" }, "繰り返しの番号（0 から）", "繰り返し");
  }
  for (const f of triggerFields(triggerSources)) {
    add({ $from: "trigger", path: pathOf([f.name]) }, `${f.description}（${f.name}）`, SOURCE_LABELS.trigger);
  }
  // 実行情報（実行した日・実行した人など・カタログの単一定義・ID は候補に出さない・D-97・D-101）。日時は日本時間。
  for (const f of RUN_INFO_CATALOG.filter((x) => x.suggested)) {
    add({ $from: "run", path: pathOf([f.name]) }, `${f.description_ja}（${f.name}）`, RUN_INFO_GROUP);
  }
  add({ $from: "input" }, `${SOURCE_LABELS.input}（全体）`, "そのほか");
  add({ $from: "trigger" }, `${SOURCE_LABELS.trigger}（全体）`, "そのほか");
  return out;
}

/// 場所を手で書くときの参照元（`$from` の閉集合・ir.md §3.1）。
export function sourceOptions({ ancestors, inMapRegion }: RefContext): { value: string; label: string }[] {
  return [
    { value: "input", label: SOURCE_LABELS.input },
    { value: "trigger", label: SOURCE_LABELS.trigger },
    { value: "run", label: SOURCE_LABELS.run },
    ...(inMapRegion ? [{ value: "each", label: SOURCE_LABELS.each }] : []),
    ...ancestors.map((a) => ({ value: `nodes.${a.id}.output`, label: `「${a.label}」の結果` })),
  ];
}

/// 場所を手で書くときの path の候補（参照元ごと）。
export function pathHints(from: string, ctx: RefContext): string[] {
  if (from === "input") return ctx.inputFields.map((f) => `/${pointerSegment(f.key)}`);
  if (from === "each") return ["/item", "/index"];
  if (from === "trigger") return triggerFields(ctx.triggerSources).map((f) => `/${pointerSegment(f.name)}`);
  if (from === "run") return RUN_INFO_CATALOG.map((f) => `/${pointerSegment(f.name)}`);
  const m = /^nodes\.(.+)\.output$/.exec(from);
  if (m) {
    const a = ctx.ancestors.find((x) => x.id === m[1]);
    return a ? outputRefFields(a).map((f) => pathOf(f.path) ?? "") : [];
  }
  return [];
}

/// 選択中の参照の表示（候補に無いものは参照元と path で書く）。
export function refLabel(ref: RefValue, ctx: RefContext): string {
  const hit = refOptions(ctx).find((o) => o.key === refKey(ref));
  if (hit) return hit.label;
  const source = sourceOptions(ctx).find((s) => s.value === ref.$from)?.label ?? ref.$from;
  return ref.path ? `${source} ${ref.path}` : source;
}

// ── 入力項目への参照（キーの変更・削除に追随する） ─────────────────────────

/// `$from: "input"` の path の先頭の段（入力項目のキー）。path が無い（入力全体）なら null。
function inputKeyOf(v: Record<string, unknown>): { key: string; rest: string } | null {
  if (v.$from !== "input" || typeof v.path !== "string" || !v.path.startsWith("/")) return null;
  const [head, ...rest] = v.path.slice(1).split("/");
  return { key: unpointerSegment(head), rest: rest.length > 0 ? `/${rest.join("/")}` : "" };
}

/// 入力項目の定義に無いキーを指す参照なら、そのキー（項目を消した・キーを書き換えた後に残った参照）。
/// 入力項目を 1 つも定義していないワークフローでは判定しない（入力の形が決まっていない）。
export function missingInputKey(ref: RefValue | null, ctx: RefContext): string | null {
  if (!ref || ctx.inputFields.length === 0) return null;
  const hit = inputKeyOf(ref as Record<string, unknown>);
  return hit && !ctx.inputFields.some((f) => f.key === hit.key) ? hit.key : null;
}

/// params の中（差し込みの vars を含む）で参照している入力項目のキー。
export function inputRefKeys(value: unknown, out: Set<string> = new Set()): Set<string> {
  if (Array.isArray(value)) {
    for (const v of value) inputRefKeys(v, out);
  } else if (value !== null && typeof value === "object") {
    const obj = value as Record<string, unknown>;
    const hit = inputKeyOf(obj);
    if (hit) out.add(hit.key);
    for (const v of Object.values(obj)) inputRefKeys(v, out);
  }
  return out;
}

/// 入力項目のキーの変更（旧キー → 新キー）に合わせて、params の中の `$from: "input"` の path の先頭の段を
/// 書き換える（差し込みの vars も）。一度に読み替えるので、2 つのキーの入れ替えでも混ざらない。
/// 何も変わらなければ同じオブジェクトを返す。
export function rewriteInputRefs(value: unknown, renames: Record<string, string>): unknown {
  if (Array.isArray(value)) {
    const next = value.map((v) => rewriteInputRefs(v, renames));
    return next.some((v, i) => v !== value[i]) ? next : value;
  }
  if (value !== null && typeof value === "object") {
    const obj = value as Record<string, unknown>;
    let changed = false;
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(obj)) {
      const next = rewriteInputRefs(v, renames);
      if (next !== v) changed = true;
      out[k] = next;
    }
    const hit = inputKeyOf(obj);
    if (hit && Object.hasOwn(renames, hit.key)) {
      out.path = `/${pointerSegment(renames[hit.key])}${hit.rest}`;
      changed = true;
    }
    return changed ? out : value;
  }
  return value;
}

function sameRef(a: unknown, ref: RefValue): boolean {
  return isFromRef(a) && a.$from === ref.$from && (a.path ?? undefined) === (ref.path ?? undefined) && a.default === undefined;
}

/// 差し込み名を path から付ける（/title → title・/items/0/name → name）。既に同じ参照があればその名前を使う。
/// 同じ名前で別の値があれば `title_2`・`title_3` と連番にする。
export function varNameFor(ref: RefValue, vars: TemplateExpr["vars"]): { name: string; reused: boolean } {
  for (const [name, v] of Object.entries(vars ?? {})) if (sameRef(v, ref)) return { name, reused: true };
  const segments = (ref.path ?? "").split("/").filter(Boolean).map(unpointerSegment);
  const last = [...segments].reverse().find((s) => !/^\d+$/.test(s));
  const node = /^nodes\.(.+)\.output$/.exec(ref.$from)?.[1];
  const raw = last ?? (segments.length > 0 ? `${node ?? ref.$from}_${segments.at(-1)}` : (node ?? ref.$from));
  const base = raw.replace(/[{}\s]/g, "_") || "v";
  let name = base;
  for (let n = 2; vars && name in vars; n += 1) name = `${base}_${n}`;
  return { name, reused: false };
}

// ── テンプレートの字句 ──────────────────────────────────────────────

export type TemplatePiece =
  | { kind: "text"; text: string }
  | { kind: "escaped"; char: "{" | "}" }
  | { kind: "var"; name: string };

/// テンプレート文字列を字句に分ける（実行時の `template_pieces` と同じ規則）。
/// エスケープ `{{`→`{`・`}}`→`}` を先に読む。`{` から次の `}` までが差し込みの名前で、閉じが無い `{` と
/// 対にならない `}` は文字のまま。
export function templatePieces(template: string): TemplatePiece[] {
  const out: TemplatePiece[] = [];
  let textStart = 0;
  let i = 0;
  const flush = (to: number) => {
    if (textStart < to) out.push({ kind: "text", text: template.slice(textStart, to) });
  };
  while (i < template.length) {
    const c = template[i];
    if ((c === "{" || c === "}") && template[i + 1] === c) {
      flush(i);
      out.push({ kind: "escaped", char: c });
      i += 2;
      textStart = i;
    } else if (c === "{") {
      const close = template.indexOf("}", i + 1);
      if (close >= 0) {
        flush(i);
        out.push({ kind: "var", name: template.slice(i + 1, close) });
        i = close + 1;
        textStart = i;
      } else {
        i += 1;
      }
    } else {
      i += 1;
    }
  }
  flush(template.length);
  return out;
}

/// 字句をテンプレート文字列に戻す。
export function joinPieces(pieces: TemplatePiece[]): string {
  return pieces
    .map((p) => (p.kind === "text" ? p.text : p.kind === "escaped" ? p.char + p.char : `{${p.name}}`))
    .join("");
}

/// 固定の文字をテンプレートの中にそのまま出す書き方にする（波括弧を二重にする）。
export const escapeTemplateText = (text: string) => text.replace(/[{}]/g, (c) => c + c);

/// テンプレートを固定値に戻したときの文字（エスケープを外し、差し込みは `{name}` のまま残す）。
export function templateToText(template: string): string {
  return templatePieces(template)
    .map((p) => (p.kind === "text" ? p.text : p.kind === "escaped" ? p.char : `{${p.name}}`))
    .join("");
}

/// テンプレートの問題（保存時の検証と同じ判定を手元で先に出す）。
/// - `missing`: vars に無い差し込み（実行すると step が失敗する）
/// - `escaped`: vars のキーを `{{key}}` と書いたもの（差し込まれず `{key}` の文字が出る）
export function templateIssues(t: TemplateExpr): { missing: string[]; escaped: string[] } {
  const vars = t.vars ?? {};
  const pieces = templatePieces(t.$template);
  const missing: string[] = [];
  const escaped: string[] = [];
  pieces.forEach((p, i) => {
    if (p.kind === "var" && !(p.name in vars) && !missing.includes(p.name)) missing.push(p.name);
    const key = escapedVarAt(pieces, i);
    if (key && key in vars && !escaped.includes(key)) escaped.push(key);
  });
  return { missing, escaped };
}

/// `{{` 文字 `}}` の並び（i が `{{` の位置）なら中の文字（前後の空白を除く）。
function escapedVarAt(pieces: TemplatePiece[], i: number): string | null {
  const [a, b, c] = [pieces[i], pieces[i + 1], pieces[i + 2]];
  if (a?.kind === "escaped" && a.char === "{" && b?.kind === "text" && c?.kind === "escaped" && c.char === "}") {
    return b.text.trim();
  }
  return null;
}

/// `{{key}}` と書いた vars のキーを差し込み `{key}` に直す。
export function fixEscapedVars(t: TemplateExpr): TemplateExpr {
  const vars = t.vars ?? {};
  const pieces = templatePieces(t.$template);
  const out: TemplatePiece[] = [];
  for (let i = 0; i < pieces.length; i += 1) {
    const key = escapedVarAt(pieces, i);
    if (key && key in vars) {
      out.push({ kind: "var", name: key });
      i += 2;
    } else {
      out.push(pieces[i]);
    }
  }
  return { ...t, $template: joinPieces(out) };
}

/// 差し込み名を変える（テンプレートの中の `{old}` も一緒に直す）。新しい名前が既にあれば変えない。
export function renameVar(t: TemplateExpr, from: string, to: string): TemplateExpr | null {
  const vars = t.vars ?? {};
  if (from === to) return t;
  if (to in vars || /[{}]/.test(to)) return null;
  // キーの並びを保つ（一覧の行が入れ替わらないように）。
  const next = Object.fromEntries(Object.entries(vars).map(([k, v]) => [k === from ? to : k, v]));
  const pieces = templatePieces(t.$template).map((p) =>
    p.kind === "var" && p.name === from ? { ...p, name: to } : p,
  );
  return { $template: joinPieces(pieces), vars: next };
}

// ── 固定値の中の差し込みのつもりの書き方 ───────────────────────────────

export type Suspect = { start: number; end: number; text: string; name: string };

/// `${x}`・`{{x}}`・`{x}`（x は名前らしい文字・`input.title` のような . 区切りも拾う）。
/// JSON の `{"a": 1}` のような文字は拾わない。
const SUSPECT_RE = /\$\{\s*([\p{L}_][\p{L}\p{N}_.]*)\s*\}|\{\{\s*([\p{L}_][\p{L}\p{N}_.]*)\s*\}\}|\{\s*([\p{L}_][\p{L}\p{N}_.]*)\s*\}/gu;

export function findSuspects(text: string): Suspect[] {
  const out: Suspect[] = [];
  for (const m of text.matchAll(SUSPECT_RE)) {
    out.push({ start: m.index, end: m.index + m[0].length, text: m[0], name: m[1] ?? m[2] ?? m[3] });
  }
  return out;
}

/// 差し込みのつもりの名前から参照を推し量る（`input.title`・`nodes.ask.output.text`・祖先の id・入力項目のキー）。
/// 当てはまらなければ入力の同じ名前の項目とみなす（変換後の差し込みの行で直せる）。
export function guessRef(name: string, ctx: RefContext): RefValue {
  const seg = name.split(".").filter(Boolean);
  const ref = (from: string, rest: string[]): RefValue => ({ $from: from, ...(rest.length ? { path: pathOf(rest) } : {}) });
  if (seg[0] === "input") return ref("input", seg.slice(1));
  if (seg[0] === "trigger") return ref("trigger", seg.slice(1));
  if (seg[0] === "each" && ctx.inMapRegion) return ref("each", seg.slice(1));
  if (seg[0] === "item" && ctx.inMapRegion) return ref("each", seg);
  if (seg[0] === "nodes" && seg.length >= 2) {
    const rest = seg.slice(2);
    return ref(`nodes.${seg[1]}.output`, rest[0] === "output" ? rest.slice(1) : rest);
  }
  const ancestor = ctx.ancestors.find((a) => a.id === seg[0]);
  if (ancestor) {
    const rest = seg.slice(1);
    return ref(`nodes.${ancestor.id}.output`, rest[0] === "output" ? rest.slice(1) : rest);
  }
  return ref("input", seg);
}

/// 固定値の文字をテンプレートにする。差し込みのつもりの書き方は差し込みに、ほかの波括弧は文字のまま
/// （二重にして）出す。
export function literalToTemplate(text: string, ctx: RefContext): TemplateExpr {
  const vars: NonNullable<TemplateExpr["vars"]> = {};
  let template = "";
  let at = 0;
  for (const s of findSuspects(text)) {
    template += escapeTemplateText(text.slice(at, s.start));
    const ref = guessRef(s.name, ctx);
    const { name, reused } = varNameFor(ref, vars);
    if (!reused) vars[name] = ref as ValueExpr;
    template += `{${name}}`;
    at = s.end;
  }
  template += escapeTemplateText(text.slice(at));
  return { $template: template, vars };
}

/// テンプレートの `start`〜`end` を差し込み `{name}` で置き換え、vars に参照を足す。
/// 返す `caret` は差し込みの直後の位置。
export function insertVar(
  t: TemplateExpr,
  range: { start: number; end: number },
  ref: RefValue,
): { template: TemplateExpr; caret: number } {
  const vars = { ...(t.vars ?? {}) };
  const { name, reused } = varNameFor(ref, vars);
  if (!reused) vars[name] = ref as ValueExpr;
  const token = `{${name}}`;
  const start = Math.max(0, Math.min(range.start, t.$template.length));
  const end = Math.max(start, Math.min(range.end, t.$template.length));
  return {
    template: { $template: t.$template.slice(0, start) + token + t.$template.slice(end), vars },
    caret: start + token.length,
  };
}

/// 固定値の文字の `start`〜`end` に差し込み、テンプレートに切り替える（前後の文字はそのまま出るように二重にする）。
export function insertIntoLiteral(
  text: string,
  range: { start: number; end: number },
  ref: RefValue,
): { template: TemplateExpr; caret: number } {
  const before = escapeTemplateText(text.slice(0, range.start));
  const after = escapeTemplateText(text.slice(range.end));
  return insertVar({ $template: before + after, vars: {} }, { start: before.length, end: before.length }, ref);
}
