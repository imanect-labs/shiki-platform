"use client";

import * as React from "react";

import {
  appendMessage,
  useChatSession,
  type ChatMessage,
} from "@/lib/chat-store";
import { mockReplyText, streamMockReply } from "@/lib/mock-assistant";
import { Composer } from "./composer";

/// 1 セッションの会話画面。store のメッセージを描画し、末尾が未応答のユーザー
/// メッセージならモック応答をストリーミング生成する。ストリーミング中の部分文字列は
/// ローカル state に保持し、完了時にのみ store へ確定保存する（localStorage への
/// 毎フレーム書き込みを避け、中断時に空応答を残さないため）。
export function Conversation({ sessionId }: { sessionId: string }) {
  const session = useChatSession(sessionId);
  const [streamingText, setStreamingText] = React.useState<string | null>(null);
  const bottomRef = React.useRef<HTMLDivElement | null>(null);
  // localStorage は client のみ。初回（SSR/ハイドレーション）は判定を保留し、
  // 「見つかりません」が一瞬ちらつくのを防ぐ。
  const [mounted, setMounted] = React.useState(false);
  React.useEffect(() => setMounted(true), []);

  const last = session?.messages[session.messages.length - 1];
  const pendingUserId = last && last.role === "user" ? last.id : null;
  const pendingText = last && last.role === "user" ? last.content : "";

  // 末尾の未応答ユーザーメッセージに対してモック応答を生成する。
  // deps はストリーミング中は変化しない（pendingUserId が安定）ため、
  // 1 セッション 1 応答で多重起動しない。cleanup で確実に停止する。
  React.useEffect(() => {
    if (!pendingUserId) return;
    setStreamingText("");
    const reply = mockReplyText(pendingText);
    const cancel = streamMockReply(
      reply,
      (partial) => setStreamingText(partial),
      () => {
        appendMessage(sessionId, "assistant", reply);
        setStreamingText(null);
      },
    );
    return cancel;
  }, [pendingUserId, pendingText, sessionId]);

  // 新着メッセージ・ストリーミング進行で最下部へ追従。
  React.useEffect(() => {
    bottomRef.current?.scrollIntoView({ block: "end" });
  }, [session?.messages.length, streamingText]);

  const handleSend = (text: string) => {
    appendMessage(sessionId, "user", text);
  };

  if (!session) {
    return (
      <div className="flex h-full items-center justify-center px-4">
        <p className="text-sm text-muted-foreground">
          {mounted ? "この会話は見つかりませんでした。" : ""}
        </p>
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col">
      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto flex w-full max-w-3xl flex-col gap-6 px-4 py-8">
          {session.messages.map((m) => (
            <MessageRow key={m.id} message={m} />
          ))}
          {streamingText !== null ? (
            <AssistantRow>
              {streamingText === "" ? (
                <TypingIndicator />
              ) : (
                <span className="whitespace-pre-wrap">
                  {streamingText}
                  <span className="ml-0.5 inline-block h-4 w-px translate-y-0.5 animate-pulse bg-foreground/60 align-middle" />
                </span>
              )}
            </AssistantRow>
          ) : null}
          <div ref={bottomRef} />
        </div>
      </div>

      <div className="border-t border-border/60 bg-background/80 backdrop-blur supports-[backdrop-filter]:bg-background/60">
        <div className="mx-auto w-full max-w-3xl px-4 py-4">
          <Composer onSubmit={handleSend} autoFocus />
          <p className="mt-2 text-center text-xs text-muted-foreground">
            これはプレビュー応答です。Shiki は誤った情報を生成することがあります。
          </p>
        </div>
      </div>
    </div>
  );
}

function MessageRow({ message }: { message: ChatMessage }) {
  if (message.role === "user") {
    return (
      <div className="flex justify-end">
        <div className="max-w-[85%] whitespace-pre-wrap rounded-2xl bg-secondary px-4 py-2.5 text-[15px] leading-relaxed text-secondary-foreground">
          {message.content}
        </div>
      </div>
    );
  }
  return (
    <AssistantRow>
      <span className="whitespace-pre-wrap">{message.content}</span>
    </AssistantRow>
  );
}

/// アシスタント発話の共通レイアウト。装飾アイコンは置かず、左寄せのプレーン文で
/// ユーザー発話（右寄せバブル）と対比させる。
function AssistantRow({ children }: { children: React.ReactNode }) {
  return (
    <div className="min-w-0 text-[15px] leading-relaxed text-foreground">{children}</div>
  );
}

function TypingIndicator() {
  return (
    <span className="inline-flex items-center gap-1" aria-label="応答を生成中">
      {[0, 1, 2].map((i) => (
        <span
          key={i}
          className="size-1.5 animate-bounce rounded-full bg-foreground/40"
          style={{ animationDelay: `${i * 0.15}s` }}
        />
      ))}
    </span>
  );
}
