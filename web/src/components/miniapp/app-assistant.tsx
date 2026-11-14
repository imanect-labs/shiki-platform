"use client";

/// ミニアプリの画面の「AI に依頼」パネル（D-10 T11・GAP-32）。
///
/// ワークフローエディタのパネル（`workflow-assistant.tsx`）と同じく [`DocChatPanel`] を、会話の対象＝このアプリ
/// （`origin_artifact` の `mini_app`）で使う。アクティブな会話は localStorage に持つ。サーバは run の開始時に、
/// このアプリの今の版を発話者の権限で読み直して AI に伝え、直す道具（miniapp_read / miniapp_update /
/// data_view_save）を最初から使える状態にする。保存の知らせ（`onBuilderSaved`）でページが画面を読み直す。
///
/// 出すのはアプリを編集できる人だけ（使う人には AI に直させる口を見せない）。保存はサーバが改めて編集の権限を
/// 確かめ、アプリの保存は承認カードを経る。

import { Loader2 } from "lucide-react";
import * as React from "react";

import { PanelEmptyHint } from "@/components/chat/doc-chat-draft";
import {
  DocChatPanel,
  useLocalActiveThreadStore,
} from "@/components/chat/doc-chat-panel";
import { useMe } from "@/hooks/use-me";
import type { BuilderRef } from "@/lib/chat-api";
import { useStorageScopeReady } from "@/lib/user-storage";

export function MiniAppChatPanel({
  appId,
  name,
  initialThreadId,
  onBuilderSaved,
}: {
  appId: string;
  /// 表示名（会話の題名の接頭に使う）。
  name: string;
  /// サイドバー等から特定会話を開くときの初期アクティブ（?thread=）。
  initialThreadId?: string | null;
  onBuilderSaved: (ref: BuilderRef) => void;
}) {
  const store = useLocalActiveThreadStore(`mini_app:${appId}`);
  // アクティブ会話 id はユーザー単位の localStorage にある（#817）。確定するまで待つ（ワークフローのパネルと同じ）。
  const scopeReady = useStorageScopeReady();
  const me = useMe();

  if (!scopeReady && me.error) {
    return (
      <div
        className="flex h-full flex-col items-center justify-center px-4 text-center text-sm"
        role="alert"
      >
        <p className="text-destructive">チャットを準備できませんでした。</p>
        <button
          type="button"
          onClick={me.reload}
          className="mt-2 text-muted-foreground underline underline-offset-2 hover:text-foreground"
        >
          再試行
        </button>
      </div>
    );
  }
  if (!scopeReady) {
    return (
      <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        チャットを準備しています…
      </div>
    );
  }
  return (
    <DocChatPanel
      store={store}
      nodeId={appId}
      originKind="mini_app"
      title={`アプリ: ${name}`}
      label="このアプリの会話"
      editable
      testIdPrefix="miniapp"
      initialThreadId={initialThreadId}
      onBuilderSaved={onBuilderSaved}
      hint={
        <PanelEmptyHint
          title="このアプリの直し方を頼めます"
          description="例:「一覧に金額の列を足して」「申請ボタンの横に取り下げボタンを付けて」。AI が保存すると、この画面に新しい下書きが読み込まれます。公開するまで、使う人の画面は変わりません。"
        />
      }
    />
  );
}
