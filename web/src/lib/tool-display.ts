/// ツール実行の表示（ラベル・アイコン・対象・フェーズ）の単一ソース（issue #386）。
///
/// **語彙の正本は codegen**（`ToolName` = `crates/agent-core/src/vocab.rs` → ts-rs）。
/// `Record<ToolName, ToolDisplay>` で束縛しているため、バックエンドにツールが増えると
/// ここが埋まるまで型エラーになる（辞書が静かに古びるのを防ぐ）。
///
/// 表示は「対象 ＋ 動詞」の具体形にする。ツール名をそのまま出さない:
///   ✗「office.live_edit を実行中」  ✓「報告書.docx を編集中」
///   ✗「ページを取得中」            ✓「example.com/ir を閲覧中」
///
/// 文言は `lead`（対象込みの前置き）＋ `verb`（連用形/サ変名詞）に分け、時制は
/// [`withTense`] が付ける。活用は語で変わる（サ変「検索しました」／和語「読み込みました」）ため
/// `suru` で切り替える — 一律に「〜しました」を付けると「読み込みしました」になる。

import {
  Blocks,
  Braces,
  Check,
  FileDown,
  FilePen,
  FilePlus,
  FileSpreadsheet,
  FileText,
  Files,
  Globe,
  LayoutTemplate,
  ListChecks,
  NotebookPen,
  Presentation,
  Search,
  Sparkles,
  Table2,
  Terminal,
  Trash2,
  Workflow,
  type LucideIcon,
} from "lucide-react";

import type { ToolName } from "@/generated/gui-spec";

/// ツールの大分類。フェーズ行の文言と季節アクセントの導出に使う
/// （旧実装は日本語ラベルのプレフィックス一致で段階判定しており、文言変更で静かに壊れていた）。
export type ToolCategory = "search" | "browse" | "read" | "write" | "exec" | "meta";

/// 入力 JSON から表示用の対象文字列を取り出す関数。
type TargetFn = (input: Record<string, unknown>) => string | null;

type ToolDisplay = {
  icon: LucideIcon;
  category: ToolCategory;
  /// 対象込みの前置き（助詞まで）。対象が取れないときは汎用名詞で埋める。
  lead: (target: string | null) => string;
  /// 動詞（連用形 or サ変名詞）。時制は withTense が付ける。
  verb: string;
  /// サ変（「検索する」型）なら true。false は和語連用形（「読み込む」型）。
  suru?: boolean;
  /// 失敗時の述語。省略時はサ変が「<verb>できませんでした」。
  /// 和語は可能形が不規則（読み込み→読み込めません）なので明示する。
  failed?: string;
  /// 入力から対象を取り出す（URL・クエリ・ファイル名など）。
  target?: TargetFn;
  /// 対象が storage の node_id で、名前解決（`node-name-cache`）が要る。
  nodeIdKey?: string;
};

/// `plan` は `ToolName` 語彙の外（`crates/agent-core/src/agent.rs` の `PLAN_TOOL` リテラル。
/// ループが横取りするため `Tool` として dispatch されない）。表示だけは同じ体系に載せる。
export const PLAN_TOOL = "plan";

// ── 入力から対象を取り出すヘルパ ────────────────────────────────

function str(input: Record<string, unknown>, key: string): string | null {
  const v = input[key];
  if (typeof v !== "string") return null;
  const t = v.trim();
  return t.length > 0 ? t : null;
}

/// 文字列を表示幅で切り詰める（末尾に「…」）。
function clip(s: string, max: number): string {
  return s.length <= max ? s : `${s.slice(0, max)}…`;
}

/// URL を「ホスト＋短いパス」に畳む。スキームと www. は落とし、長いパスは末尾を省略する。
/// 例 `https://www.example.com/a/very/long/path?q=1` → `example.com/a/very/lo…`
export function shortUrl(raw: string): string {
  let u: URL;
  try {
    u = new URL(raw);
  } catch {
    return clip(raw, 44);
  }
  const host = u.hostname.replace(/^www\./, "");
  const path = u.pathname === "/" ? "" : u.pathname;
  return clip(`${host}${path}`, 44);
}

/// 検索クエリなどの自由文を鉤括弧つきで返す。
function quoted(key: string, max = 30): TargetFn {
  return (input) => {
    const v = str(input, key);
    return v ? `「${clip(v, max)}」` : null;
  };
}

/// 生の文字列（コマンド・SQL）。改行は畳んで長さを抑える。
function inline(key: string, max = 40): TargetFn {
  return (input) => {
    const v = str(input, key);
    return v ? clip(v.replace(/\s+/g, " "), max) : null;
  };
}

function named(key = "name", max = 40): TargetFn {
  return (input) => {
    const v = str(input, key);
    return v ? clip(v, max) : null;
  };
}

const urlTarget: TargetFn = (input) => {
  const v = str(input, "url");
  return v ? shortUrl(v) : null;
};

/// 「<対象> を」形。対象が無ければ汎用名詞で埋める。
/// 対象（ファイル名・URL）のときだけ助詞の前に空きを入れて可読性を上げ、
/// 汎用名詞のときは詰める（「ノート を読み込み」と間延びさせない）。
function objectOf(fallbackNoun: string) {
  return (t: string | null) => (t ? `${t} を` : `${fallbackNoun}を`);
}

// ── 辞書（30 語彙・漏れはコンパイルエラー）──────────────────────

const TOOL_DISPLAY: Record<ToolName, ToolDisplay> = {
  skill: {
    icon: Sparkles,
    category: "meta",
    lead: (t) => (t ? `スキル「${t}」を` : "スキルを"),
    verb: "読み込み",
    failed: "読み込めませんでした",
    target: named(),
  },
  doc_search: {
    icon: Search,
    category: "search",
    lead: (t) => (t ? `${t}で社内文書を` : "社内文書を"),
    verb: "検索",
    suru: true,
    target: quoted("query"),
  },
  web_search: {
    icon: Globe,
    category: "search",
    lead: (t) => (t ? `${t}を web で` : "web を"),
    verb: "検索",
    suru: true,
    target: quoted("query"),
  },
  web_fetch: {
    icon: FileDown,
    category: "browse",
    lead: objectOf("ページ"),
    verb: "閲覧",
    suru: true,
    target: urlTarget,
  },
  code_interpreter: {
    icon: Terminal,
    category: "exec",
    lead: () => "コードを",
    verb: "実行",
    suru: true,
  },

  // 自律エージェントのワークスペース操作。
  fs_list: { icon: Files, category: "read", lead: () => "ファイル一覧を", verb: "取得", suru: true },
  fs_read: {
    icon: FileText,
    category: "read",
    lead: objectOf("ファイル"),
    verb: "読み込み",
    failed: "読み込めませんでした",
    target: named(),
  },
  grep: {
    icon: Search,
    category: "search",
    lead: (t) => (t ? `${t}でファイルを` : "ファイルを"),
    verb: "検索",
    suru: true,
    target: quoted("pattern"),
  },
  fs_write: {
    icon: FilePlus,
    category: "write",
    lead: (t) => (t ? `${t} に` : "ファイルに"),
    verb: "書き込み",
    failed: "書き込めませんでした",
    target: named(),
  },
  fs_edit: {
    icon: FilePen,
    category: "write",
    lead: objectOf("ファイル"),
    verb: "編集",
    suru: true,
    target: named(),
  },
  fs_delete: {
    icon: Trash2,
    category: "write",
    lead: objectOf("ファイル"),
    verb: "削除",
    suru: true,
    target: named(),
  },
  shell: {
    icon: Terminal,
    category: "exec",
    lead: objectOf("コマンド"),
    verb: "実行",
    suru: true,
    target: inline("cmd"),
  },

  // 画面・ワークフロー。
  emit_ui: { icon: LayoutTemplate, category: "write", lead: () => "画面を", verb: "組み立て", failed: "組み立てられませんでした" },
  emit_workflow: {
    icon: Workflow,
    category: "write",
    lead: () => "ワークフローを",
    verb: "保存",
    suru: true,
  },
  read_workflow: {
    icon: Workflow,
    category: "read",
    lead: () => "ワークフローを",
    verb: "読み込み",
    failed: "読み込めませんでした",
  },

  // ノート（md ドキュメント）。
  "document.read": {
    icon: NotebookPen,
    category: "read",
    lead: objectOf("ノート"),
    verb: "読み込み",
    failed: "読み込めませんでした",
    nodeIdKey: "node_id",
  },
  "document.edit": {
    icon: FilePen,
    category: "write",
    lead: objectOf("ノート"),
    verb: "編集",
    suru: true,
    nodeIdKey: "node_id",
  },
  "document.embed": {
    icon: Blocks,
    category: "write",
    lead: (t) => (t ? `${t} に図表を` : "ノートに図表を"),
    verb: "埋め込み",
    failed: "埋め込めませんでした",
    nodeIdKey: "node_id",
  },

  // 下書き確定型の作成系（保存はユーザーの確定操作）。
  save_note: {
    icon: NotebookPen,
    category: "write",
    lead: (t) => (t ? `ノート「${t}」の下書きを` : "ノートの下書きを"),
    verb: "作成",
    suru: true,
    target: named(),
  },
  save_slide: {
    icon: Presentation,
    category: "write",
    lead: (t) => (t ? `スライド「${t}」の下書きを` : "スライドの下書きを"),
    verb: "作成",
    suru: true,
    target: named(),
  },
  save_csv: {
    icon: Table2,
    category: "write",
    lead: (t) => (t ? `CSV「${t}」の下書きを` : "CSV の下書きを"),
    verb: "作成",
    suru: true,
    target: named(),
  },
  save_document: {
    icon: FileText,
    category: "write",
    lead: (t) => (t ? `「${t}」を Word で` : "Word 文書を"),
    verb: "作成",
    suru: true,
    target: named(),
  },
  save_sheet: {
    icon: FileSpreadsheet,
    category: "write",
    lead: (t) => (t ? `「${t}」を Excel で` : "Excel ブックを"),
    verb: "作成",
    suru: true,
    target: named(),
  },

  // スライド。
  "slide.read": {
    icon: Presentation,
    category: "read",
    lead: objectOf("スライド"),
    verb: "読み込み",
    failed: "読み込めませんでした",
    nodeIdKey: "node_id",
  },
  "slide.edit": {
    icon: Presentation,
    category: "write",
    lead: objectOf("スライド"),
    verb: "編集",
    suru: true,
    nodeIdKey: "node_id",
  },

  // Office（Collabora）。
  "office.edit": {
    icon: FilePen,
    category: "write",
    lead: objectOf("Office ファイル"),
    verb: "編集",
    suru: true,
    nodeIdKey: "node_id",
  },
  "office.live_edit": {
    icon: FilePen,
    category: "write",
    lead: objectOf("Office ファイル"),
    verb: "ライブ編集",
    suru: true,
    nodeIdKey: "node_id",
  },

  // CSV（表・SQL 分析）。
  "csv.query": {
    icon: Braces,
    category: "read",
    lead: (t) => (t ? `SQL「${t}」を CSV に` : "CSV に SQL を"),
    verb: "実行",
    suru: true,
    target: inline("sql", 34),
  },
  "csv.patch": {
    icon: Table2,
    category: "write",
    lead: objectOf("CSV ファイル"),
    verb: "更新",
    suru: true,
    nodeIdKey: "node_id",
  },
  "csv.write": {
    icon: Table2,
    category: "write",
    lead: (t) => (t ? `「${t}」を CSV で` : "CSV を"),
    verb: "作成",
    suru: true,
    target: named(),
  },
};

const PLAN_DISPLAY: ToolDisplay = {
  icon: ListChecks,
  category: "meta",
  lead: () => "計画を",
  verb: "更新",
  suru: true,
};

const UNKNOWN_DISPLAY: ToolDisplay = {
  icon: Check,
  category: "meta",
  lead: () => "処理を",
  verb: "実行",
  suru: true,
};

function displayFor(name: string): ToolDisplay {
  if (name === PLAN_TOOL) return PLAN_DISPLAY;
  return TOOL_DISPLAY[name as ToolName] ?? UNKNOWN_DISPLAY;
}

// ── 公開 API ───────────────────────────────────────────────────

/// 表示に必要な最小の入力（`ToolActivityItem` の部分集合）。
export type ToolDescribable = {
  name: string;
  input?: unknown;
};

function asRecord(input: unknown): Record<string, unknown> | null {
  return input && typeof input === "object" && !Array.isArray(input)
    ? (input as Record<string, unknown>)
    : null;
}

/// このツールが node_id からのファイル名解決を要するか（要るなら node_id を返す）。
export function nodeIdOf(item: ToolDescribable): string | null {
  const meta = displayFor(item.name);
  if (!meta.nodeIdKey) return null;
  const input = asRecord(item.input);
  if (!input) return null;
  const v = input[meta.nodeIdKey];
  return typeof v === "string" && v.length > 0 ? v : null;
}

export type ToolDescription = {
  icon: LucideIcon;
  category: ToolCategory;
  /// 「報告書.docx を編集」のような対象込みの文（述語なし）。
  lead: string;
  /// 動詞（連用形 or サ変名詞）。
  verb: string;
  /// サ変か（[`withTense`] が活用を選ぶ）。
  suru: boolean;
  /// 失敗時の述語。
  failed: string;
};

/// ツール 1 件の表示を組み立てる。`nodeName` は node_id 解決済みの名前（未解決なら null）。
export function describeTool(item: ToolDescribable, nodeName?: string | null): ToolDescription {
  const meta = displayFor(item.name);
  const input = asRecord(item.input);
  const target = nodeName ?? (input && meta.target ? meta.target(input) : null);
  const suru = meta.suru === true;
  return {
    icon: meta.icon,
    category: meta.category,
    lead: meta.lead(target),
    verb: meta.verb,
    suru,
    // サ変の既定は「<動詞>できませんでした」。和語は可能形が不規則なので辞書側で明示する。
    failed: meta.failed ?? `${meta.verb}できませんでした`,
  };
}

/// 述語を付ける。サ変は「検索しました」、和語連用形は「読み込みました」。
/// 「〜しました」を一律に付けると「読み込みしました」になるため語で分ける。
/// 失敗時は完了形にしない（「取得に失敗したのに閲覧しました」と言わせない）。
export function withTense(d: ToolDescription, running: boolean, failed = false): string {
  if (running) return `${d.lead}${d.verb}中`;
  if (failed) return `${d.lead}${d.failed}`;
  return d.suru ? `${d.lead}${d.verb}しました` : `${d.lead}${d.verb}ました`;
}

const CATEGORY_PHASE: Record<ToolCategory, string> = {
  search: "検索しています",
  browse: "ページを読んでいます",
  read: "内容を読み込んでいます",
  write: "書き込んでいます",
  exec: "実行しています",
  meta: "準備しています",
};

/// フェーズ行の文言。直近のツールの category から決める
/// （呼び出し側が計画のサブタスクを持っていればそちらを優先する）。
export function phaseLabel(items: ToolDescribable[], running: boolean): string {
  if (items.length === 0) return running ? "準備しています" : "実行内容";
  return CATEGORY_PHASE[displayFor(items[items.length - 1].name).category];
}

/// 折りたたみ 1 行要約用の内訳（「web 8 ・ 社内 3」）。
export function summarizeTools(items: ToolDescribable[]): string[] {
  let web = 0;
  let internal = 0;
  let edits = 0;
  for (const it of items) {
    const meta = displayFor(it.name);
    if (it.name === "web_search" || it.name === "web_fetch") web += 1;
    else if (it.name === "doc_search") internal += 1;
    else if (meta.category === "write" || meta.category === "exec") edits += 1;
  }
  const parts: string[] = [];
  if (web > 0) parts.push(`web ${web}`);
  if (internal > 0) parts.push(`社内 ${internal}`);
  if (edits > 0) parts.push(`編集・実行 ${edits}`);
  return parts;
}

/// 進行段階 → 季節アクセント（春=準備 / 夏=検索・閲覧 / 秋=書き込み / 冬=完了）。
/// 文言ではなく category から導くので、ラベルを変えても壊れない。
export function seasonIndexFor(category: ToolCategory, running: boolean): number {
  if (!running) return 3; // 冬（完了後は落ち着かせる）
  switch (category) {
    case "search":
    case "browse":
      return 1; // 夏
    case "write":
      return 2; // 秋
    default:
      return 0; // 春
  }
}
