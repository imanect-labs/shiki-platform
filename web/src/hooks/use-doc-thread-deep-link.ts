"use client";

/// 文書画面（ノート・Office）の `?thread=` ディープリンクとアシスタントの開閉（#598）。
///
/// - `?thread=` で来たらアシスタントを開き、その会話を指定する。同じ画面のまま `?thread=` が
///   変わったとき（履歴・検索から同じ文書の別の会話を選んだとき）も開き直して指定する。
/// - パネルを閉じたらディープリンクは**使い終わり**にする。開き直したときは保存済みの選択
///   （パネル内で手動で切り替えた会話を含む）に従い、URL の会話へ戻さない。

import * as React from "react";

export function useDocThreadDeepLink(threadParam: string | null) {
  const [chatOpen, setChatOpen] = React.useState(Boolean(threadParam));
  const [deepLinkThreadId, setDeepLinkThreadId] = React.useState(threadParam);

  React.useEffect(() => {
    if (!threadParam) return;
    setDeepLinkThreadId(threadParam);
    setChatOpen(true);
  }, [threadParam]);

  React.useEffect(() => {
    if (!chatOpen) setDeepLinkThreadId(null);
  }, [chatOpen]);

  return { chatOpen, setChatOpen, deepLinkThreadId };
}
