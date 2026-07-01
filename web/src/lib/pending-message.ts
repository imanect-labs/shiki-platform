/// ホームで作成したスレッドの「最初のメッセージ」を会話画面へ受け渡す一時ストア。
/// sessionStorage に置き、会話画面マウント時に 1 度だけ取り出して送信する。

import type { Attachment } from "@/lib/chat-api";

const KEY = "shiki:pending-message:";

export type PendingMessage = { text: string; attachments: Attachment[] };

export function stashPending(threadId: string, msg: PendingMessage): void {
  if (typeof sessionStorage === "undefined") return;
  try {
    sessionStorage.setItem(KEY + threadId, JSON.stringify(msg));
  } catch {
    /* 容量超過等は無視 */
  }
}

export function popPending(threadId: string): PendingMessage | null {
  if (typeof sessionStorage === "undefined") return null;
  try {
    const raw = sessionStorage.getItem(KEY + threadId);
    if (!raw) return null;
    sessionStorage.removeItem(KEY + threadId);
    return JSON.parse(raw) as PendingMessage;
  } catch {
    return null;
  }
}
