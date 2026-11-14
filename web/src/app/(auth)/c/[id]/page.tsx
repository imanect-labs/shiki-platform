"use client";

import * as React from "react";
import { use } from "react";

import { Conversation } from "@/components/chat/conversation";
import { getThread, setThreadOriginArtifact, type BuilderRef } from "@/lib/chat-api";

/// 会話画面 `/c/[id]`。Next 15 では params は Promise のため use() で展開する。
/// スレッド本体は backend（/threads）から取得し、応答は SSE でストリーミングする。
export default function ChatPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = use(params);
  const onBuilderSaved = useAttachBuiltApp(id);
  return <Conversation threadId={id} onBuilderSaved={onBuilderSaved} />;
}

/// 「AI と作る」（目的 app_builder）の会話でアプリができたら、その会話をアプリに紐づける（D-10 T11）。
///
/// 以後、サイドバーの履歴からはアプリの画面（「AI に依頼」のパネル）で開き、AI はそのアプリを対象に続ける。
/// 紐づけるのは最初にできた 1 つだけで、対象をすでに持つ会話は付け替えない。サーバは会話の owner にだけ許すので、
/// 共有された会話の編集者が開いていても付け替わらない（失敗は黙って捨てる・会話はそのまま続けられる）。
function useAttachBuiltApp(threadId: string): (ref: BuilderRef) => void {
  // 紐づけてよい会話か（目的 app_builder・対象なし）。読めるまでは紐づけない。
  const attachable = React.useRef<Promise<boolean> | null>(null);
  const attached = React.useRef(false);
  React.useEffect(() => {
    attached.current = false;
    attachable.current = getThread(threadId)
      .then((t) => t.purpose === "app_builder" && !t.originArtifactId)
      .catch(() => false);
  }, [threadId]);
  return React.useCallback(
    (ref: BuilderRef) => {
      if (ref.kind !== "mini_app" || attached.current || !attachable.current) return;
      attached.current = true;
      void attachable.current.then((ok) =>
        ok
          ? setThreadOriginArtifact(threadId, { kind: "mini_app", id: ref.id }).catch(() => {
              /* owner でない・アプリが見えない: 紐づけずに、この画面で続ける */
            })
          : undefined,
      );
    },
    [threadId],
  );
}
