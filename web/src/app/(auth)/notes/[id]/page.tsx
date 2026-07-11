"use client";

/// ノートページ（Task 11P.3）。/notes/[id] で .md ファイルを共同編集する。
///
/// - 接続は CollabProvider（BFF セッション Cookie・y-websocket 互換ワイヤ）。
/// - 権限はサーバが強制する（viewer の update 不受理・剥奪切断）。ここでは
///   /collab/docs/{id}/access の表示用ヒントで editable を切り替える。
/// - 11P.5 で本ページが「ノート×チャット分割ビュー」のホストになる。

import { ArrowLeft, Eye, Loader2 } from "lucide-react";
import Link from "next/link";
import { useParams } from "next/navigation";
import * as React from "react";
import * as Y from "yjs";

import { MetadataPanel } from "@/components/notes/metadata-panel";
import { NoteEditor } from "@/components/notes/note-editor";
import { PresenceAvatars } from "@/components/notes/presence";
import { EmptyState } from "@/components/ui/empty-state";
import { useMe } from "@/hooks/use-me";
import { CollabProvider, type CollabStatus } from "@/lib/collab";
import { getCollabAccess, type CollabAccess } from "@/lib/notes-api";

/// 接続状態のラベル（保存はサーバ側デバウンス・切断時のみ注意を促す）。
function statusLabel(status: CollabStatus, synced: boolean): string {
  if (status === "connected") return synced ? "同期済み" : "同期中…";
  if (status === "connecting") return "接続中…";
  return "オフライン（再接続します）";
}

export default function NotePage() {
  const params = useParams<{ id: string }>();
  const nodeId = params.id;
  const me = useMe();
  const [access, setAccess] = React.useState<CollabAccess | null | "notfound" | "loading">(
    "loading",
  );
  const [status, setStatus] = React.useState<CollabStatus>("connecting");
  const [synced, setSynced] = React.useState(false);

  // Yjs ドキュメントとプロバイダ（ノート単位で 1 つ・アンマウントで破棄）。
  const [session, setSession] = React.useState<{
    doc: Y.Doc;
    provider: CollabProvider;
  } | null>(null);

  React.useEffect(() => {
    let cancelled = false;
    getCollabAccess(nodeId)
      .then((a) => {
        if (!cancelled) setAccess(a ?? "notfound");
      })
      .catch(() => {
        if (!cancelled) setAccess("notfound");
      });
    return () => {
      cancelled = true;
    };
  }, [nodeId]);

  React.useEffect(() => {
    if (typeof access === "string" || access === null) return;
    const doc = new Y.Doc();
    const provider = new CollabProvider(nodeId, doc);
    const offStatus = provider.onStatus(setStatus);
    const offSynced = provider.onSynced(() => setSynced(true));
    setSession({ doc, provider });
    return () => {
      offStatus();
      offSynced();
      provider.destroy();
      doc.destroy();
      setSession(null);
      setSynced(false);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nodeId, typeof access === "string" ? access : "ready"]);

  if (access === "loading" || me.loading) {
    return (
      <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        ノートを開いています…
      </div>
    );
  }
  if (access === "notfound" || access === null) {
    return (
      <EmptyState
        title="ノートが見つかりません"
        description="削除されたか、アクセス権がありません。"
      />
    );
  }

  const editable = access.mode === "editor";
  const userId = me.data?.id ?? "unknown";
  const userName = me.data?.email?.split("@")[0] ?? userId;

  return (
    <div className="mx-auto flex h-full w-full max-w-3xl flex-col px-4">
      <header className="flex items-center gap-3 py-3">
        <Link
          href="/drive"
          className="flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          aria-label="ドライブへ戻る"
        >
          <ArrowLeft className="size-4" />
        </Link>
        <span className="min-w-0 flex-1 truncate text-sm text-muted-foreground">
          {access.name}
        </span>
        {!editable && (
          <span
            className="inline-flex items-center gap-1 rounded-full border bg-muted/50 px-2.5 py-0.5 text-xs font-medium text-muted-foreground"
            data-testid="note-readonly-badge"
          >
            <Eye className="size-3.5" aria-hidden />
            閲覧のみ
          </span>
        )}
        {session && <PresenceAvatars provider={session.provider} />}
        <span
          className="text-xs text-muted-foreground tabular-nums"
          data-testid="note-sync-status"
        >
          {statusLabel(status, synced)}
        </span>
      </header>

      {session && (
        <div className="flex-1 overflow-y-auto pb-24">
          <MetadataPanel meta={session.doc.getMap("meta")} editable={editable} />
          <div className="mt-4">
            <NoteEditor
              provider={session.provider}
              editable={editable}
              user={{ id: userId, name: userName }}
            />
          </div>
        </div>
      )}
    </div>
  );
}
