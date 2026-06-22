"use client";

// BFF 認証。ブラウザにトークンは置かず、code 受け／token 交換／refresh は
// すべてサーバ側（shiki-server の /auth/*）が担う。フロントはログイン開始と
// ログアウト要求をするだけ（docs/auth/browser-token-strategy.md）。

import { csrfToken } from "@/lib/api";

/// ログイン開始。BFF の /auth/login へフル遷移する（→ Keycloak へリダイレクト）。
export function login(): void {
  window.location.href = "/auth/login";
}

/// ログアウト。CSRF 付きで BFF に通知し、返却された Keycloak end-session URL へ遷移する。
export async function logout(): Promise<void> {
  const token = csrfToken();
  const res = await fetch("/auth/logout", {
    method: "POST",
    credentials: "include",
    headers: token ? { "X-CSRF-Token": token } : {},
  });
  if (res.ok) {
    const body = (await res.json()) as { end_session_url: string };
    window.location.href = body.end_session_url;
  } else {
    // ログアウト要求が弾かれてもトップへ戻す（セッションは Cookie 失効で実質無効）。
    window.location.href = "/";
  }
}
