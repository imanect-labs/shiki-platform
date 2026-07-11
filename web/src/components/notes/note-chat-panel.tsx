"use client";

/// ノートに紐づくチャットパネル（Task 11P.5・分割ビューのサイドパネル）。
///
/// - 紐づくスレッド id は**ノートの Yjs Map "meta" に thread_id** として保持する
///   （ノート共同編集者間で共有される）。無ければ初回オープン時に作成して meta に書く。
/// - 既存チャットスレッド UI（`Conversation`）を**そのまま再利用**する（分割ビューの
///   実装は一箇所のみ）。
/// - ノート共有とスレッド共有は**別 ReBAC**。スレッド閲覧権限が無い共同編集者には
///   `Conversation` が fail-closed で「見つかりません」を表示する（暗黙共有しない）。

import { Loader2, X } from "lucide-react";
import * as React from "react";
import type * as Y from "yjs";

import { Conversation } from "@/components/chat/conversation";
import { Button } from "@/components/ui/button";
import { createThread } from "@/lib/chat-api";

/// meta から thread_id を読む（文字列のみ）。
function readThreadId(meta: Y.Map<unknown>): string | null {
  const v = meta.get("thread_id");
  return typeof v === "string" && v.length > 0 ? v : null;
}

export function NoteChatPanel({
  meta,
  noteName,
  editable,
  onClose,
}: {
  meta: Y.Map<unknown>;
  noteName: string;
  /// editor のみがスレッドを新規作成できる（viewer は既存スレッドの閲覧のみ）。
  editable: boolean;
  onClose: () => void;
}) {
  const [threadId, setThreadId] = React.useState<string | null>(() => readThreadId(meta));
  const [error, setError] = React.useState<string | null>(null);
  // 作成の多重発火を防ぐ ref（StrictMode の二重実行・再マウントに耐える）。
  const creatingRef = React.useRef(false);

  // meta の thread_id 変化（他の共同編集者が作成）に追従する。
  React.useEffect(() => {
    const update = () => setThreadId(readThreadId(meta));
    meta.observe(update);
    return () => meta.unobserve(update);
  }, [meta]);

  // スレッド未作成なら（editor のとき）作成して meta に書く。ref ガードで一度だけ。
  React.useEffect(() => {
    if (threadId || !editable || creatingRef.current) return;
    creatingRef.current = true;
    createThread(`ノート: ${noteName}`)
      .then((thread) => {
        // meta.set が observe を発火し setThreadId まで至る（cancelled で捨てない）。
        meta.set("thread_id", thread.id);
        setThreadId(thread.id);
      })
      .catch(() => {
        creatingRef.current = false;
        setError("チャットの準備に失敗しました。");
      });
  }, [threadId, editable, meta, noteName]);

  return (
    <aside
      className="flex h-full min-h-0 w-full flex-col border-l bg-background"
      aria-label="ノートのチャット"
      data-testid="note-chat-panel"
    >
      <header className="flex items-center gap-2 border-b px-3 py-2">
        <span className="flex-1 text-sm font-medium">アシスタント</span>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          onClick={onClose}
          aria-label="チャットを閉じる"
          className="size-8"
        >
          <X className="size-4" />
        </Button>
      </header>
      <div className="min-h-0 flex-1">
        {threadId ? (
          <Conversation threadId={threadId} />
        ) : error ? (
          <div className="flex h-full items-center justify-center px-4 text-center text-sm text-muted-foreground">
            {error}
          </div>
        ) : editable ? (
          <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" aria-hidden />
            チャットを準備しています…
          </div>
        ) : (
          <div className="flex h-full items-center justify-center px-4 text-center text-sm text-muted-foreground">
            まだチャットが開始されていません。
          </div>
        )}
      </div>
    </aside>
  );
}
