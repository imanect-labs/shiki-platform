/// ホーム（や文書のアシスタントパネル）で作成したスレッドの「最初のメッセージ」を会話画面へ受け渡す一時ストア。
/// sessionStorage に置き、会話画面マウント時に 1 度だけ取り出して送信する。

import type { Attachment } from "@/lib/chat-api";
import type { SelectionContext } from "@/lib/selection-context";
import { storageScopeGeneration } from "@/lib/user-storage";

const KEY = "shiki:pending-message:";

export type PendingMessage = {
  text: string;
  attachments: Attachment[];
  /// ホームで選んだエージェントモード（初回メッセージへ確実に反映する・state の初期化タイミングに依存しない）。
  autonomous?: boolean;
  /// **この発話にだけ**適用する skill（スラッシュコマンド起動・#387）。ピンではない。
  skills?: { artifactId: string; version?: number | null }[];
  /// エディタの選択コンテキスト（文書のアシスタントパネルで最初の発話に添えた選択箇所）。
  context?: SelectionContext;
};

/// `startedAt` は送信を始めたときの [`storageScopeGeneration`]。スレッド作成を待つ間に
/// ログアウト（他タブを含む）があれば、消した後に本文・添付を書き戻さない（#817）。
export function stashPending(threadId: string, msg: PendingMessage, startedAt: number): void {
  if (typeof sessionStorage === "undefined") return;
  if (startedAt !== storageScopeGeneration()) return;
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
