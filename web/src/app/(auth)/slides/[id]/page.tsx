"use client";

/// スライドページ（Task 11.1）。/slides/[id] で .slide ファイルを表示する。
///
/// - 接続は CollabProvider（BFF セッション Cookie・y-websocket 互換ワイヤ）。真実は Yjs。
/// - 本 PR は閲覧ビューのみ。編集（GrapesJS 砂箱エディタ・Task 11.2）は本ページの
///   editable 分岐に載る。描画は SlideFrame（DOMPurify＋sandbox iframe・PIT-40）。

import { Loader2 } from "lucide-react";
import { useParams } from "next/navigation";
import * as React from "react";
import * as Y from "yjs";

import { SlideHeaderSlot } from "@/components/slides/slide-header-slot";
import { SlideWorkspace } from "@/components/slides/slide-workspace";
import { EmptyState } from "@/components/ui/empty-state";
import { CollabProvider, type CollabStatus } from "@/lib/collab";
import { getCollabAccess, type CollabAccess } from "@/lib/notes-api";

export default function SlidePage() {
  const params = useParams<{ id: string }>();
  const nodeId = params.id;
  const [access, setAccess] = React.useState<CollabAccess | null | "notfound" | "loading">(
    "loading",
  );
  const [status, setStatus] = React.useState<CollabStatus>("connecting");
  const [synced, setSynced] = React.useState(false);
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

  if (access === "loading") {
    return (
      <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        スライドを開いています…
      </div>
    );
  }
  if (access === "notfound" || access === null) {
    return (
      <EmptyState
        title="スライドが見つかりません"
        description="削除されたか、アクセス権がありません。"
      />
    );
  }

  const editable = access.mode === "editor";

  return (
    <div className="flex h-full min-h-0 flex-col">
      <SlideHeaderSlot
        name={access.name.replace(/\.slide$/i, "")}
        editable={editable}
        status={status}
        synced={synced}
        provider={session?.provider ?? null}
      />
      <div className="min-h-0 flex-1">
        {session && synced ? (
          <SlideWorkspace
            doc={session.doc}
            editable={editable}
            name={access.name.replace(/\.slide$/i, "")}
          />
        ) : (
          <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" aria-hidden />
            同期しています…
          </div>
        )}
      </div>
    </div>
  );
}
