"use client";

/// ノートの印刷ビュー（#334・PDF エクスポート）。読み取り専用で本文を全幅描画し、
/// 同期完了後に window.print() を呼ぶ（ブラウザの「PDF に保存」で PDF 化する）。
///
/// human 決定: PDF はブラウザ印刷ベース。チャート（recharts SVG）はそのままベクタ印刷され、
/// iframe 埋め込みは印刷用プレースホルダへ差し替わる（embed-view の print スタイル）。
/// アプリシェル（サイドバー/ヘッダ）は print:hidden で除外する。

import { EditorLoading } from "@/components/shell/editor-loading";
import { useParams } from "next/navigation";
import * as React from "react";
import * as Y from "yjs";

import { MetadataPanel } from "@/components/notes/metadata-panel";
import { NoteEditor } from "@/components/notes/note-editor";
import { EmptyState } from "@/components/ui/empty-state";
import { CollabProvider } from "@/lib/collab";
import { getCollabAccess, type CollabAccess } from "@/lib/notes-api";

export default function NotePrintPage() {
  const params = useParams<{ id: string }>();
  const nodeId = params.id;
  const [access, setAccess] = React.useState<CollabAccess | null | "notfound" | "loading">(
    "loading",
  );
  const [session, setSession] = React.useState<{ doc: Y.Doc; provider: CollabProvider } | null>(
    null,
  );
  const printedRef = React.useRef(false);

  React.useEffect(() => {
    let cancelled = false;
    getCollabAccess(nodeId)
      .then((a) => !cancelled && setAccess(a ?? "notfound"))
      .catch(() => !cancelled && setAccess("notfound"));
    return () => {
      cancelled = true;
    };
  }, [nodeId]);

  React.useEffect(() => {
    if (typeof access === "string" || access === null) return;
    const doc = new Y.Doc();
    const provider = new CollabProvider(nodeId, doc);
    // 同期完了後に一度だけ印刷ダイアログを出す（描画確定を少し待つ）。
    const offSynced = provider.onSynced(() => {
      if (printedRef.current) return;
      printedRef.current = true;
      setTimeout(() => window.print(), 800);
    });
    setSession({ doc, provider });
    return () => {
      offSynced();
      provider.destroy();
      doc.destroy();
      setSession(null);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nodeId, typeof access === "string" ? access : "ready"]);

  if (access === "loading") {
    return <EditorLoading kind="doc" message="印刷プレビューを準備しています…" />;
  }
  if (access === "notfound" || access === null) {
    return <EmptyState title="ノートが見つかりません" description="削除されたか、アクセス権がありません。" />;
  }

  return (
    <div className="note-print mx-auto max-w-3xl px-6 py-8" data-testid="note-print">
      {session ? (
        <>
          <h1 className="mb-4 text-2xl font-bold">{access.name.replace(/\.md$/i, "")}</h1>
          <MetadataPanel meta={session.doc.getMap("meta")} editable={false} />
          <div className="mt-4">
            {/* 読み取り専用（editable=false）で本文を描画。genui/チャートはそのまま印刷される。 */}
            <NoteEditor
              provider={session.provider}
              editable={false}
              user={{ id: "print", name: "print" }}
            />
          </div>
        </>
      ) : (
        <EditorLoading kind="doc" message="本文を同期しています…" />
      )}
    </div>
  );
}
