"use client";

/// AI が作成/編集した文書の参照カード（#381・document_ref ブロック）。
///
/// これが無いと「AI が Excel を 2 回編集して v3 にした」作業が思考プロセス内の畳まれた
/// チップだけになり、成果物への導線がどこにも無い（#358 の実害）。note_ref カードと同型で、
/// 遷移先は**サーバが決めた kind**（拡張子判定はサーバ側 1 箇所）に従う。

import {
  ArrowRight,
  Check,
  Copy,
  FileSpreadsheet,
  FileText,
  NotebookPen,
  Presentation,
} from "lucide-react";
import Link from "next/link";
import * as React from "react";

export type DocumentRef = {
  id: string;
  name: string;
  /// office / note / slide / csv / file（サーバの document_ref::kind_for が正）。
  kind: string;
  /// 版が確定しているときのみ。ライブ編集で永続化未確認なら null。
  version: number | null;
  /// 新規作成なら true（フロントはこのときだけ自動遷移する）。
  created: boolean;
  /// 提案バージョン（WOPI ロック中の office.edit・PIT-44）なら true。
  /// current には**まだ入っていない**ので「編集しました」と言い切らない。
  proposal: boolean;
};

/// 参照 JSON を防御的にパースする（形が崩れていたら描画しない）。
export function parseDocumentRef(raw: unknown): DocumentRef | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.id !== "string" || typeof r.name !== "string") return null;
  return {
    id: r.id,
    name: r.name,
    kind: typeof r.kind === "string" ? r.kind : "file",
    version: typeof r.version === "number" ? r.version : null,
    created: r.created === true,
    proposal: r.proposal === true,
  };
}

/// 種別ごとの遷移先。**専用エディタを持たない種別は `null`**（リンクを出さない）。
/// 「開く」を押したのに何も開かない／関係ない画面に飛ぶ、を作らないため。
export function documentRefHref(doc: DocumentRef): string | null {
  switch (doc.kind) {
    case "office":
      return `/office/${doc.id}`;
    case "note":
      return `/notes/${doc.id}`;
    case "slide":
      return `/slides/${doc.id}`;
    case "csv":
      return `/csv/${doc.id}`;
    default:
      return null;
  }
}

function KindIcon({ kind, name }: { kind: string; name: string }) {
  const cls = "size-4.5";
  if (kind === "note") return <NotebookPen className={cls} aria-hidden />;
  if (kind === "slide") return <Presentation className={cls} aria-hidden />;
  if (kind === "csv" || /\.xlsx$/i.test(name)) {
    return <FileSpreadsheet className={cls} aria-hidden />;
  }
  return <FileText className={cls} aria-hidden />;
}

export function DocumentRefCard({ raw }: { raw: unknown }) {
  const doc = parseDocumentRef(raw);
  if (!doc) return null;
  // 提案版は current に反映されていない。「編集しました」と言うとカードが嘘になる（PIT-44）。
  const action = doc.proposal
    ? "提案として保存しました（バージョン履歴から採用すると反映されます）"
    : doc.created
      ? "作成しました"
      : "編集しました";
  const version = doc.version === null ? "" : `（v${doc.version}）`;
  const href = documentRefHref(doc);
  return (
    <div
      className="my-2 flex items-center gap-3 rounded-xl border bg-card p-3"
      data-testid="document-ref-card"
    >
      <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary">
        <KindIcon kind={doc.kind} name={doc.name} />
      </span>
      <span className="min-w-0 flex-1">
        <span className="truncate text-sm font-medium">{doc.name}</span>
        <span className="mt-0.5 block text-xs text-muted-foreground">
          {doc.proposal ? `${version}${action}` : `${action}${version}`}
        </span>
      </span>
      {href ? (
        <Link
          href={href}
          className="inline-flex shrink-0 items-center gap-1.5 rounded-full border px-3 py-1.5 text-xs font-medium transition-colors duration-fast hover:border-primary/40 hover:bg-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          開く
          <ArrowRight className="size-3.5" aria-hidden />
        </Link>
      ) : null}
    </div>
  );
}

/// **レガシー**: 廃止した下書き Word 文書カード（#332 → #381 で撤去）。
///
/// 過去スレッドに残る `document_draft` ブロックを黙って消さないための読み取り専用表示。
/// 遷移先（`/office/draft`）も下書きストアも既に無いが、**本文はブロックに残っている**ため、
/// 展開して読める＋コピーできるようにする（アップグレード時点の未保存下書きを取り戻せなくしない）。
export function LegacyDocumentDraftCard({ raw }: { raw: unknown }) {
  const draft =
    typeof raw === "object" && raw !== null
      ? (raw as { name?: unknown; markdown?: unknown })
      : null;
  const name = typeof draft?.name === "string" ? draft.name : null;
  const markdown = typeof draft?.markdown === "string" ? draft.markdown : "";
  const [copied, setCopied] = React.useState(false);
  if (!name) return null;
  return (
    <div
      className="my-2 rounded-xl border border-dashed bg-muted/30 p-3"
      data-testid="legacy-document-draft-card"
    >
      <div className="flex items-center gap-3">
        <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground">
          <FileText className="size-4.5" aria-hidden />
        </span>
        <span className="min-w-0 flex-1">
          <span className="truncate text-sm font-medium text-muted-foreground">{name}</span>
          <span className="mt-0.5 block text-xs text-muted-foreground">
            この下書き機能は廃止されました（Word は作成時にそのまま .docx になります）。本文は
            下に残っています。
          </span>
        </span>
        {markdown ? (
          <button
            type="button"
            onClick={() => {
              void navigator.clipboard.writeText(markdown).then(() => {
                setCopied(true);
                window.setTimeout(() => setCopied(false), 1500);
              });
            }}
            className="inline-flex shrink-0 items-center gap-1.5 rounded-full border px-3 py-1.5 text-xs font-medium transition-colors duration-fast hover:border-primary/40 hover:bg-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            {copied ? <Check className="size-3.5" aria-hidden /> : <Copy className="size-3.5" aria-hidden />}
            {copied ? "コピーしました" : "本文をコピー"}
          </button>
        ) : null}
      </div>
      {markdown ? (
        <details className="mt-2">
          <summary className="cursor-pointer text-xs text-muted-foreground hover:text-foreground">
            本文を表示
          </summary>
          <pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap rounded-lg bg-background p-2.5 text-xs">
            {markdown}
          </pre>
        </details>
      ) : null}
    </div>
  );
}
