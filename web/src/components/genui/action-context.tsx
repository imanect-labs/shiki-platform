"use client";

/// generative UI のアクション実行コンテキスト（Task 6.5/6.6）。
///
/// レンダラ配下のフォーム/ボタンは **`action_id` と `params` だけ**をここへ渡す。
/// 実際の HTTP 呼び出し先（チャットのメッセージ由来か・ミニアプリ由来か）は Provider が
/// 閉じ込め、**UI 側に任意 URL への fetch の口は存在しない**。

import * as React from "react";

import {
  invokeChatUiAction,
  invokeMiniAppUiAction,
  type UiActionResult,
} from "@/lib/artifact-api";

export type GenUiDispatch = (actionId: string, params: unknown) => Promise<UiActionResult>;

type GenUiActionContextValue = {
  dispatch: GenUiDispatch;
  /// いま**即時に**実行できるか。false は「押せない」ではなく「押したら順番待ちに入る」。
  ///
  /// アクションの照合は確定メッセージに保存された spec に対して行うため、生成中のカードは
  /// その場では実行できない。以前は押下時に「生成が完了してから実行できます」と投げていたが、
  /// 生成が終わるとカードは確定メッセージ側へ作り直され、**入力済みの回答がすべて消えていた**
  /// （実 LLM 検証で、3 問答えてから押した回答が丸ごと失われた）。いまは受理して積み、
  /// 生成が終わった瞬間に自動で送る。カードはこの値で「順番待ち」だけ見せる。
  ready: boolean;
  /// アクション成功後のフック（chat.submit 後の会話リフレッシュ等）。
  onActionCompleted?: (result: UiActionResult) => void;
};

const GenUiActionContext = React.createContext<GenUiActionContextValue | null>(null);

/// アクション未配線の描画（プレビュー等）。押下時に明示エラーにする。
const noopDispatch: GenUiDispatch = async () => {
  throw new Error("この画面ではアクションを実行できません");
};

export function useGenUiAction(): GenUiActionContextValue {
  return React.useContext(GenUiActionContext) ?? { dispatch: noopDispatch, ready: false };
}

/// チャットメッセージ内の generative_ui ブロック用 Provider。
export function ChatGenUiProvider({
  threadId,
  messageId,
  onQueue,
  onActionCompleted,
  children,
}: {
  threadId: string;
  /// 確定メッセージの id。ストリーミング中（未確定）は null＝その場では実行できない。
  messageId: string | null;
  /// 未確定のときの受け皿。生成が終わってから実行するために積む（渡されなければ従来どおり
  /// 押下時エラー）。積んだ時点でユーザーには「受理された」と見せる。
  onQueue?: (actionId: string, params: unknown) => void;
  onActionCompleted?: (result: UiActionResult) => void;
  children: React.ReactNode;
}) {
  const value = React.useMemo<GenUiActionContextValue>(
    () => ({
      dispatch: async (actionId, params) => {
        if (messageId) return invokeChatUiAction(threadId, messageId, actionId, params);
        if (!onQueue) throw new Error("生成が完了してから実行できます");
        onQueue(actionId, params);
        // 積んだことを成功として返す（カードは押した瞬間に受理表示へ移る）。実際の実行は
        // 生成完了後で、そこで新しい発話と生成が生まれる＝会話側で見える。
        return { result: { kind: "queued" } } as unknown as UiActionResult;
      },
      ready: messageId !== null,
      onActionCompleted,
    }),
    [threadId, messageId, onQueue, onActionCompleted],
  );
  return <GenUiActionContext.Provider value={value}>{children}</GenUiActionContext.Provider>;
}

/// ミニアプリ実行画面用 Provider（解決済み版に固定して実行する）。
export function MiniAppGenUiProvider({
  appId,
  version,
  onActionCompleted,
  children,
}: {
  appId: string;
  version: number;
  onActionCompleted?: (result: UiActionResult) => void;
  children: React.ReactNode;
}) {
  const value = React.useMemo<GenUiActionContextValue>(
    () => ({
      dispatch: (actionId, params) => invokeMiniAppUiAction(appId, version, actionId, params),
      // ミニアプリは解決済みの版に対して実行するので、常に実行できる。
      ready: true,
      onActionCompleted,
    }),
    [appId, version, onActionCompleted],
  );
  return <GenUiActionContext.Provider value={value}>{children}</GenUiActionContext.Provider>;
}
