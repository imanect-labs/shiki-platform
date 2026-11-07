/// ホームの入力欄の上に置くモード（チャット／社内検索／エージェント／作成・#964）。
///
/// モードは「最初の発話をどう始めるか」だけを決め、始まったスレッドの中では通常の会話になる。
/// - チャット: 通常のチャット（モデルが必要に応じてツールを使う）。
/// - 社内検索: この発話に社内文書検索（doc_search）を必ず使う（tool-choice と同じ仕組み）。
/// - エージェント: エージェントモードで始める（作業フォルダ・承認モードを選べる）。
/// - 作成: 作るものの種類を本文の先頭に添えて送る（AI が対応する保存ツールで下書きを作る）。

import type { Attachment } from "@/lib/chat-api";
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
  /// 入力欄のプレースホルダ。作成モードは種類ごとの文言を使う。
  placeholder: string;
  /// スマホ（1 行の入力欄）向けの短いプレースホルダ。
  short: string;
};

export const HOME_MODES: readonly HomeModeDef[] = [
  {
    key: "chat",
    label: "チャット",
    icon: MessageSquareText,
    placeholder: "何でも尋ねて、社内文書も検索",
    short: "Shiki に質問・依頼",
  },
  {
    key: "search",
    label: "社内検索",
    icon: Search,
    placeholder: "社内の文書から探して答えます（見る権限のある文書だけ）",
    short: "社内の文書から探す",
  },
  {
    key: "agent",
    label: "エージェント",
    icon: Bot,
    placeholder: "やってほしいことを書いてください。計画を立て、確認しながら進めます",
    short: "エージェントに依頼",
  },
  {
    key: "create",
    label: "作成",
    icon: NotebookPen,
    placeholder: "作りたいものを書いてください",
    short: "作りたいもの",
  },
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
  /// スマホ（1 行の入力欄）向けの短いプレースホルダ。
  short: string;
};

export const CREATE_KINDS: readonly CreateKindDef[] = [
  {
    key: "note",
    label: "ノート",
    icon: NotebookPen,
    lead: "ノートで作成",
    placeholder: "ノートで書きたいことを入力（例: 新製品の企画メモ）",
    short: "ノートで作りたいもの",
  },
  {
    key: "document",
    label: "ドキュメント",
    icon: FileText,
    lead: "ドキュメント（Word）で作成",
    placeholder: "ドキュメントで作りたいものを入力（例: 取引先への提案書）",
    short: "ドキュメントで作りたいもの",
  },
  {
    key: "sheet",
    label: "Excel",
    icon: FileSpreadsheet,
    lead: "Excel で作成",
    placeholder: "Excel で作りたいものを入力（例: 月次の経費集計表）",
    short: "Excel で作りたいもの",
  },
  {
    key: "slide",
    label: "スライド",
    icon: Presentation,
    lead: "スライドで作成",
    placeholder: "スライドで作りたいものを入力（例: 第3四半期の営業報告 5 枚）",
    short: "スライドで作りたいもの",
  },
  {
    key: "csv",
    label: "CSV",
    icon: Sheet,
    lead: "CSV で作成",
    placeholder: "CSV で作りたいものを入力（例: 取引先一覧のひな形）",
    short: "CSV で作りたいもの",
  },
];

export function createKindDef(kind: CreateKind): CreateKindDef {
  return CREATE_KINDS.find((k) => k.key === kind) ?? CREATE_KINDS[0];
}

/// 作成モードで送る本文（種類を先頭に添える）。利用者が見る会話の発話もこの文になる。
export function createPrompt(kind: CreateKind, text: string): string {
  return `${createKindDef(kind).lead}：${text.trim()}`;
}

/// 候補プロンプト（押すと即その文で始める）。
export type Suggestion = {
  /// 送る本文。
  text: string;
  /// チップに出す文（未指定なら `text`）。
  label?: string;
  /// 出どころの小さな注記。
  meta?: string;
  /// 一緒に添付する文書（最近共有された文書から作った候補）。
  attachment?: Attachment;
};

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

/// チップに出す文書名。拡張子と、文字の向きを変える制御文字などの書式文字（Cf）を除き、長すぎれば詰める。
/// 文書名は共有した人が自由に付けられるので、見た目を偽る文字を画面に出さない。
function docLabel(name: string): string {
  const base = name.replace(/\p{Cf}/gu, "").replace(/\.[^./]+$/u, "") || name;
  return base.length > 30 ? `${base.slice(0, 29)}…` : base;
}

/// モードの候補を作る。チャット・社内検索では、最近自分に共有された文書から先頭に 2 つまで作り、
/// 残りを既定の候補で埋める（共有が無い人にも 4 つ出る）。
///
/// 文書から作る候補は、文書を**添付**して決まった文で尋ねる。文書名は共有した人が付けた任意の文字列
/// なので、利用者の発話の本文には入れない（名前に紛れた指示が利用者の言葉として届かないように）。
export function homeSuggestions(
  mode: HomeMode,
  sharedFiles: readonly { id: string; name: string }[],
): Suggestion[] {
  const docs = sharedFiles.slice(0, 2);
  const fromShares: Suggestion[] =
    mode === "chat"
      ? docs.map((d) => ({
          text: "添付した文書の要点を教えて",
          label: `「${docLabel(d.name)}」の要点を教えて`,
          meta: "共有された文書",
          attachment: { node_id: d.id, name: d.name },
        }))
      : mode === "search"
        ? docs.map((d) => ({
            text: "添付した文書に関係する社内の資料を探して",
            label: `「${docLabel(d.name)}」に関係する資料を探して`,
            meta: "共有された文書",
            attachment: { node_id: d.id, name: d.name },
          }))
        : [];
  return [...fromShares, ...DEFAULT_SUGGESTIONS[mode]].slice(0, 4);
}
