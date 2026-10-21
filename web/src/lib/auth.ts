"use client";

// BFF 認証。ブラウザにトークンは置かず、code 受け／token 交換／refresh は
// すべてサーバ側（shiki-server の /auth/*）が担う。フロントはログイン開始と
// ログアウト要求をするだけ（docs/auth/browser-token-strategy.md）。

import { csrfToken } from "@/lib/api";

/// ログイン後に戻る先を OIDC ラウンドトリップ跨ぎで保持するための sessionStorage キー。
/// BFF の /auth/callback は常に "/" へ戻すため、戻り先はフロントで覚えておく。
const POST_LOGIN_NEXT_KEY = "shiki:post-login-next";

/// ログイン開始。戻り先を控えてから BFF の /auth/login へフル遷移する（→ Keycloak）。
export function login(next?: string): void {
  if (next && next !== "/" && typeof sessionStorage !== "undefined") {
    sessionStorage.setItem(POST_LOGIN_NEXT_KEY, next);
  }
  window.location.href = "/auth/login";
}

/// /login が Keycloak へ自動で送った時刻の控え（直近の数件）。ログイン済みになったら消す。
const AUTO_LOGIN_AT_KEY = "shiki:auto-login-at";
/// この時間内に自動で送った回数を数える。
const AUTO_LOGIN_WINDOW_MS = 10_000;
/// 時間内にこの回数送っても未ログインのまま戻ってくるなら、ループとみなして止める。
/// ログアウト直後に保護ページを開き直す（2 回目）のような通常の往復は止めない。
const AUTO_LOGIN_MAX_IN_WINDOW = 2;

/// /login から Keycloak へ自動で送ってよいか。短時間に何度送っても未ログインで戻ってくる場合
/// （Cookie が保存されない設定など）は false を返し、リダイレクトの無限ループを止める。
/// 送ってよいときは時刻を控える。sessionStorage が使えないときは常に送る（従来どおり）。
export function claimAutoLogin(): boolean {
  try {
    const now = Date.now();
    const raw = JSON.parse(sessionStorage.getItem(AUTO_LOGIN_AT_KEY) ?? "[]") as unknown;
    const recent = (Array.isArray(raw) ? raw : []).filter(
      (t): t is number => typeof t === "number" && now - t < AUTO_LOGIN_WINDOW_MS,
    );
    if (recent.length >= AUTO_LOGIN_MAX_IN_WINDOW) return false;
    sessionStorage.setItem(AUTO_LOGIN_AT_KEY, JSON.stringify([...recent, now]));
  } catch {
    // ストレージが使えない環境では止め役を持たない。
  }
  return true;
}

/// 控えておいたログイン後の戻り先を取り出して消費する（無ければ null）。
/// ログイン済みで呼ばれるので、自動ログインの控えもここで消す。
export function consumePostLoginNext(): string | null {
  if (typeof sessionStorage === "undefined") return null;
  try {
    sessionStorage.removeItem(AUTO_LOGIN_AT_KEY);
  } catch {
    // 消せなくても控えは 10 秒で数えなくなるだけ。
  }
  const next = sessionStorage.getItem(POST_LOGIN_NEXT_KEY);
  if (next) sessionStorage.removeItem(POST_LOGIN_NEXT_KEY);
  return next;
}

/// ログイン状態を確認する（401 を出さない /auth/session を使う）。
/// 失敗時は安全側に倒して false（未ログイン扱い）を返す。
export async function checkSession(): Promise<boolean> {
  try {
    const res = await fetch("/auth/session", { credentials: "include" });
    if (!res.ok) return false;
    const body = (await res.json()) as { authenticated?: boolean };
    return body.authenticated === true;
  } catch {
    return false;
  }
}

/// ログアウト。CSRF 付きで BFF に通知し、返却された Keycloak end-session URL へ遷移する。
export async function logout(): Promise<void> {
  const token = csrfToken();
  try {
    const res = await fetch("/auth/logout", {
      method: "POST",
      credentials: "include",
      headers: token ? { "X-CSRF-Token": token } : {},
    });
    if (res.ok) {
      const body = (await res.json()) as { end_session_url: string };
      window.location.href = body.end_session_url;
      return;
    }
    // ログアウト要求が弾かれてもトップへ戻す（セッションは Cookie 失効で実質無効）。
    window.location.href = "/";
  } catch {
    // オフライン・DNS 失敗などネットワーク例外でも握り潰さずトップへ誘導する。
    window.location.href = "/";
  }
}
