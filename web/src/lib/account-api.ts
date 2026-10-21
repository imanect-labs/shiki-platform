import type { components } from "@/generated/api";
import { apiFetch } from "@/lib/api";

/// OpenAPI(utoipa) から生成した型（手書きの対応物を作らない）。
export type SecurityOverview = components["schemas"]["SecurityOverview"];
export type CredentialInfo = components["schemas"]["CredentialInfo"];
export type SessionsResponse = components["schemas"]["SessionsResponse"];
export type DeviceInfo = components["schemas"]["DeviceInfo"];
export type SessionInfo = components["schemas"]["SessionInfo"];
export type AccountActionKind = components["schemas"]["AccountActionKind"];

/// ログイン方法・端末の API エラー（画面にそのまま出す文言を持つ）。
export class AccountApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

async function check(res: Response): Promise<Response> {
  if (res.ok) return res;
  // 画面を開いたままセッションが切れた場合。AuthGate はマウント時にしか確認しないので、
  // ここでログインへ送る（戻り先は今の画面）。
  if (res.status === 401 && typeof window !== "undefined") {
    const next = `${window.location.pathname}${window.location.search}`;
    window.location.assign(`/login?next=${encodeURIComponent(next)}`);
  }
  const message =
    res.status === 401
      ? "ログインの有効期限が切れました。もう一度ログインしてください"
      : res.status === 503
      ? "認証サーバ（Keycloak）に接続できません"
      : res.status === 404
        ? "対象のセッションが見つかりません（すでに終了している可能性があります）"
        : `エラーが発生しました（${res.status}）`;
  throw new AccountApiError(res.status, message);
}

/// 自分のログイン方法（パスワード・パスキー・認証アプリ）。
export async function fetchSecurity(): Promise<SecurityOverview> {
  return (await (await check(await apiFetch("/me/security"))).json()) as SecurityOverview;
}

/// 自分の端末とセッション。
export async function fetchSessions(): Promise<SessionsResponse> {
  return (await (await check(await apiFetch("/me/sessions"))).json()) as SessionsResponse;
}

/// 指定したセッションからログアウトさせる。
export async function signOutSession(id: string): Promise<void> {
  await check(await apiFetch(`/me/sessions/${encodeURIComponent(id)}`, { method: "DELETE" }));
}

/// このブラウザ以外の全セッションからログアウトさせる。
export async function signOutOtherSessions(): Promise<void> {
  await check(await apiFetch("/me/sessions", { method: "DELETE" }));
}

/// Keycloak の画面で行うアカウント操作へ移動する（パスキー登録・OTP 設定・パスワード変更・削除）。
/// BFF が authorize へ 302 し、完了後は /settings/security?action=…&status=… に戻る。
export function startAccountAction(action: AccountActionKind, credentialId?: string): void {
  const params = new URLSearchParams({ action });
  if (credentialId) params.set("credential_id", credentialId);
  window.location.assign(`/auth/action?${params.toString()}`);
}
