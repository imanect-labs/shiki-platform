"use client";

/// ホスト支援 PKCE の popup callback（Task 9.11）。
///
/// Keycloak からの authorization code を受け取り、**opener（同一オリジンのシェル）へ
/// postMessage** して自分は閉じる。token 交換はシェル側（code_verifier を持つ）が行う。
/// このページ自身はコードを保存も交換もしない。

import * as React from "react";

export default function MiniAppOAuthCallbackPage() {
  const [message, setMessage] = React.useState("認可応答を処理しています…");

  React.useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const code = params.get("code");
    const state = params.get("state");
    const error = params.get("error");
    if (!window.opener) {
      setMessage("このページは認可ポップアップからのみ使用されます。");
      return;
    }
    if (error || !code) {
      setMessage(`認可に失敗しました: ${error ?? "コードなし"}`);
    }
    // 同一オリジンのシェルにだけ届ける（origin 固定・混線防止）。
    (window.opener as Window).postMessage(
      { type: "shiki:oauth-code", code, state },
      window.location.origin,
    );
    window.close();
  }, []);

  return (
    <div className="flex min-h-40 items-center justify-center text-sm text-muted-foreground">
      {message}
    </div>
  );
}
