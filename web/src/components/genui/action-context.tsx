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
  /// いまアクションを実行できるか。ストリーミング中のカードは **まだ実行できない**
  /// （アクションの照合は確定メッセージに保存された spec に対して行われるため）。
  ///
  /// これを**押してから知らせる**のではなく**押す前に見せる**ために公開している。
  /// 以前は押下時に「生成が完了してから実行できます」と投げていたが、生成が終わると
  /// カードは確定メッセージ側へ作り直され、**入力済みの回答がすべて消えていた**
  /// （実 LLM 検証で、3 問答えてから押した回答が丸ごと失われた）。
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
  onActionCompleted,
  children,
}: {
  threadId: string;
  /// 確定メッセージの id。ストリーミング中（未確定）は null＝実行不可（保存後に有効化）。
  messageId: string | null;
  onActionCompleted?: (result: UiActionResult) => void;
  children: React.ReactNode;
}) {
  const value = React.useMemo<GenUiActionContextValue>(
    () => ({
      dispatch: async (actionId, params) => {
        if (!messageId) throw new Error("生成が完了してから実行できます");
        return invokeChatUiAction(threadId, messageId, actionId, params);
      },
      ready: messageId !== null,
      onActionCompleted,
    }),
    [threadId, messageId, onActionCompleted],
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
