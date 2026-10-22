"use client";

/// Office 文書（Collabora）に紐づくチャットパネル（Task 11.10）。
///
/// 汎用 [`DocChatPanel`] に、アクティブ会話 id を **localStorage** へ保存する
/// [`ActiveThreadStore`] を与えた薄いラッパ。Office 文書は Yjs を持たない（Collabora が
/// バイナリを持つ）ため、会話継続は per-user の localStorage で担保する。会話一覧・1:N は
/// ノートと同じく `origin_note_id` = ファイルの node id で引く。

import * as React from "react";
import { Loader2 } from "lucide-react";

import { DocChatPanel, useLocalActiveThreadStore } from "@/components/chat/doc-chat-panel";
import { useMe } from "@/hooks/use-me";
import { officeTargetKind } from "@/lib/office/chart-insert";
import { useStorageScopeReady } from "@/lib/user-storage";

import { OfficeTargetProvider } from "./office-target";

export function OfficeChatPanel({
  fileId,
  fileName,
  initialThreadId,
}: {
  fileId: string;
  fileName: string;
  /// サイドバー等から特定会話を開くときの初期アクティブ（?thread=・#598）。
  initialThreadId?: string | null;
}) {
  const store = useLocalActiveThreadStore(fileId);
  const cleanName = React.useMemo(() => fileName.replace(/\.(docx|xlsx|pptx|odt|ods|odp)$/i, ""), [fileName]);
  // 回答の図（棒・折れ線・円）を、この文書へグラフとして入れられるようにする（#731）。
  const target = React.useMemo(() => {
    const kind = officeTargetKind(fileName);
    return kind ? { fileId, kind } : null;
  }, [fileId, fileName]);
  // アクティブ会話 id はユーザー単位の localStorage にある（#817）。ユーザーが確定する前に
  // 読むと「保存値なし」とみなして新しい会話を作ってしまうため、確定まで待つ。
  const scopeReady = useStorageScopeReady();
  // /me が 401 以外（403・5xx・ネットワーク断）で失敗するとスコープは確定しない。待ち続けず、
  // 失敗を伝えて取り直せるようにする（401 はログイン画面へ送られるので待ち表示のままでよい）。
  const me = useMe();
  if (!scopeReady && me.error) {
    return (
      <div className="flex h-full flex-col items-center justify-center px-4 text-center text-sm" role="alert">
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
    <OfficeTargetProvider target={target}>
      <DocChatPanel
        store={store}
        nodeId={fileId}
        title={`文書: ${cleanName}`}
        label="この文書の会話"
        // 文書を開けている（＝viewer 以上）なら会話を作れる。AI の編集自体は office.edit が
        // editor@file をサーバ側で再判定するため、ここを editor に限定しない。
        editable
        testIdPrefix="office"
        initialThreadId={initialThreadId}
      />
    </OfficeTargetProvider>
  );
}
