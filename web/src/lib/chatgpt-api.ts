import type { components } from "@/generated/api";
import { apiFetch } from "@/lib/api";

/// OpenAPI(utoipa) から生成した型（手書きの対応物を作らない）。
export type ChatGptStatus = components["schemas"]["ChatGptStatusResponse"];
export type ChatGptConnectionState = components["schemas"]["ChatGptConnectionState"];
export type ChatGptModelInfo = components["schemas"]["ChatGptModelInfo"];

/// ChatGPT 連携の API エラー（画面にそのまま出す文言を持つ）。
export class ChatGptApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

async function check(res: Response): Promise<Response> {
  if (res.ok) return res;
  if (res.status === 401 && typeof window !== "undefined") {
    const next = `${window.location.pathname}${window.location.search}`;
    window.location.assign(`/login?next=${encodeURIComponent(next)}`);
  }
  // 400（貼り間違い・期限切れ・許可していない）と 409（別の利用者）はサーバの文言をそのまま出す。
  let detail: string | undefined;
  try {
    const body = (await res.json()) as { detail?: unknown };
    if (typeof body.detail === "string") detail = body.detail;
  } catch {
    // 本文が JSON でなければ既定の文言にする。
  }
  const message =
    detail ??
    (res.status === 401
      ? "ログインの有効期限が切れました。もう一度ログインしてください"
      : res.status === 404
        ? "この環境では ChatGPT 連携が有効になっていません"
        : res.status === 503
          ? "ChatGPT の認証サーバと通信できませんでした。しばらくしてからもう一度お試しください"
          : `エラーが発生しました（${res.status}）`);
  throw new ChatGptApiError(res.status, message);
}

/// 自分の ChatGPT プランの接続状態。
export async function fetchChatGptStatus(): Promise<ChatGptStatus> {
  return (await (await check(await apiFetch("/me/chatgpt"))).json()) as ChatGptStatus;
}

/// ChatGPT でのログインを始め、ブラウザで開く認可 URL を受け取る。
export async function startChatGptLogin(): Promise<string> {
  const res = await check(await apiFetch("/me/chatgpt/login", { method: "POST" }));
  return ((await res.json()) as components["schemas"]["ChatGptLoginResponse"]).authorize_url;
}

/// 許可のあとブラウザが開いた URL を渡して、接続を完了する。
export async function completeChatGptLogin(callbackUrl: string): Promise<ChatGptStatus> {
  const body: components["schemas"]["ChatGptCallbackRequest"] = { callback_url: callbackUrl };
  const res = await check(
    await apiFetch("/me/chatgpt/callback", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }),
  );
  return (await res.json()) as ChatGptStatus;
}

/// 接続を外す。
export async function disconnectChatGpt(): Promise<void> {
  await check(await apiFetch("/me/chatgpt", { method: "DELETE" }));
}
