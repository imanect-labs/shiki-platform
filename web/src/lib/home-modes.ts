/// ホームの入力欄の上に置くモード（チャット／社内検索／エージェント／作成・#964）。
///
/// モードは「最初の発話をどう始めるか」だけを決め、始まったスレッドの中では通常の会話になる。
/// - チャット: 通常のチャット（モデルが必要に応じてツールを使う）。
/// - 社内検索: この発話に社内文書検索（doc_search）を必ず使う（tool-choice と同じ仕組み）。
/// - エージェント: エージェントモードで始める（作業フォルダ・承認モードを選べる）。
/// - 作成: 作るものの種類を本文の先頭に添えて送る（AI が対応する保存ツールで下書きを作る）。

import {
  Bot,
  FileSpreadsheet,
  FileText,
  MessageSquareText,
  NotebookPen,
  Presentation,
  Search,
  Sheet,
  type LucideIcon,
} from "lucide-react";

export type HomeMode = "chat" | "search" | "agent" | "create";

export type HomeModeDef = {
  key: HomeMode;
  label: string;
  icon: LucideIcon;
  /// 入力欄のプレースホルダ（PC）。作成モードは種類ごとの文言を使う。
  placeholder: string;
};

export const HOME_MODES: readonly HomeModeDef[] = [
  { key: "chat", label: "チャット", icon: MessageSquareText, placeholder: "何でも尋ねて、社内文書も検索" },
  {
    key: "search",
    label: "社内検索",
    icon: Search,
    placeholder: "社内の文書から探して答えます（見る権限のある文書だけ）",
  },
  {
    key: "agent",
    label: "エージェント",
    icon: Bot,
    placeholder: "やってほしいことを書いてください。計画を立て、確認しながら進めます",
  },
  { key: "create", label: "作成", icon: NotebookPen, placeholder: "作りたいものを書いてください" },
];

export function homeModeDef(mode: HomeMode): HomeModeDef {
  return HOME_MODES.find((m) => m.key === mode) ?? HOME_MODES[0];
}

/// 作成モードで選べる種類。並びは「+」メニューの作成（composer-plus-menu）と揃える。
export type CreateKind = "note" | "document" | "sheet" | "slide" | "csv";

export type CreateKindDef = {
  key: CreateKind;
  label: string;
  icon: LucideIcon;
  /// 送る本文の先頭に添える語（AI にどの形式で作るかを伝える。会話にもそのまま表示される）。
  lead: string;
  placeholder: string;
};

export const CREATE_KINDS: readonly CreateKindDef[] = [
  {
    key: "note",
    label: "ノート",
    icon: NotebookPen,
    lead: "ノートで作成",
    placeholder: "ノートで書きたいことを入力（例: 新製品の企画メモ）",
  },
  {
    key: "document",
    label: "ドキュメント",
    icon: FileText,
    lead: "ドキュメント（Word）で作成",
    placeholder: "ドキュメントで作りたいものを入力（例: 取引先への提案書）",
  },
  {
    key: "sheet",
    label: "Excel",
    icon: FileSpreadsheet,
    lead: "Excel で作成",
    placeholder: "Excel で作りたいものを入力（例: 月次の経費集計表）",
  },
  {
    key: "slide",
    label: "スライド",
    icon: Presentation,
    lead: "スライドで作成",
    placeholder: "スライドで作りたいものを入力（例: 第3四半期の営業報告 5 枚）",
  },
  {
    key: "csv",
    label: "CSV",
    icon: Sheet,
    lead: "CSV で作成",
    placeholder: "CSV で作りたいものを入力（例: 取引先一覧のひな形）",
  },
];

export function createKindDef(kind: CreateKind): CreateKindDef {
  return CREATE_KINDS.find((k) => k.key === kind) ?? CREATE_KINDS[0];
}

/// 作成モードで送る本文（種類を先頭に添える）。利用者が見る会話の発話もこの文になる。
export function createPrompt(kind: CreateKind, text: string): string {
  return `${createKindDef(kind).lead}：${text.trim()}`;
}

/// 候補プロンプト（押すと即その文で始める）。`meta` は出どころの小さな注記。
export type Suggestion = { text: string; meta?: string };

/// 実データ（最近共有された文書）が無いときにも出す、モードごとの既定の候補。
export const DEFAULT_SUGGESTIONS: Record<HomeMode, readonly Suggestion[]> = {
  chat: [
    { text: "社内の経費精算のルールを教えて" },
    { text: "この資料をもとにメール文面を作成して" },
    { text: "新機能の企画アイデアを5つ出して" },
    { text: "会議の議事録を要点だけにまとめて" },
  ],
  search: [
    { text: "出張旅費の規程を探して" },
    { text: "情報セキュリティ研修の資料はどこ？" },
    { text: "経費精算の締め日はいつ？" },
    { text: "取引先との契約書のひな形を探して" },
  ],
  agent: [
    { text: "競合3社の新製品を調べて比較表にまとめて" },
    { text: "先週の議事録から ToDo を担当者ごとに整理して" },
    { text: "社内の規程を読んで新人向けの FAQ を作って" },
    { text: "このフォルダの資料を要約して一覧にして" },
  ],
  create: [
    { text: "新製品の企画書" },
    { text: "月次の経費集計表" },
    { text: "第3四半期の営業報告" },
    { text: "取引先一覧のひな形" },
  ],
};

/// 拡張子を外した文書名（候補の文に入れる）。
function docTitle(name: string): string {
  return name.replace(/\.[^./]+$/u, "") || name;
}

/// モードの候補を作る。チャット・社内検索では、最近自分に共有された文書から先頭に 2 つまで作り、
/// 残りを既定の候補で埋める（共有が無い人にも 4 つ出る）。
export function homeSuggestions(mode: HomeMode, sharedFileNames: readonly string[]): Suggestion[] {
  const docs = sharedFileNames.slice(0, 2).map(docTitle);
  const fromShares: Suggestion[] =
    mode === "chat"
      ? docs.map((n) => ({ text: `「${n}」の要点を教えて`, meta: "共有された文書" }))
      : mode === "search"
        ? docs.map((n) => ({ text: `「${n}」に関係する資料を探して`, meta: "共有された文書" }))
        : [];
  return [...fromShares, ...DEFAULT_SUGGESTIONS[mode]].slice(0, 4);
}
