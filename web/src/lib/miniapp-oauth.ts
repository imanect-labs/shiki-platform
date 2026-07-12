/// B1 ミニアプリのホスト支援 PKCE（Task 9.11・human 確定判断）。
///
/// out-of-trust の iframe（opaque origin）は自分で OAuth リダイレクトを完結できないため、
/// **ホストシェルが popup で authorization code を取得**し、token 交換までを代行して
/// アクセストークンだけを postMessage で手渡す。refresh token はシェル側にも保持しない
/// （アルファ: 失効したらアプリが再要求 → popup 再取得。B1 client の access token は短命 5 分）。
///
/// セキュリティ境界:
/// - code_verifier はシェルのメモリのみ（iframe へ渡らない）
/// - popup callback は同一オリジン（/apps/oauth/callback）→ opener へ postMessage（origin 検証）
/// - state はリクエストごとの乱数で CSRF/混線を防ぐ

"use client";

import { sha256Hex } from "@/lib/sha256";

/// Keycloak realm の issuer（ブラウザから到達可能な URL）。
export function oidcIssuer(): string {
  return process.env.NEXT_PUBLIC_OIDC_ISSUER ?? "http://localhost:8081/realms/shiki";
}

function randomUrlSafe(bytes: number): string {
  const buf = new Uint8Array(bytes);
  crypto.getRandomValues(buf);
  return btoa(String.fromCharCode(...buf)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

async function codeChallenge(verifier: string): Promise<string> {
  const hex = await sha256Hex(new TextEncoder().encode(verifier).buffer as ArrayBuffer);
  // hex → bytes → base64url（S256 チャレンジ形式）。
  const bytes = new Uint8Array(hex.match(/.{2}/g)!.map((b) => parseInt(b, 16)));
  return btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export type GatewayToken = { accessToken: string; expiresIn: number };

/// popup で authorization code を取得し、token 交換して access token を返す。
///
/// `scopes` はミニアプリが要求する能力スコープ（granted の範囲内で有効・ゲートウェイが
/// granted ∩ token を強制する）。
export async function acquireGatewayToken(
  clientIdB1: string,
  scopes: string[],
): Promise<GatewayToken> {
  const issuer = oidcIssuer();
  const redirectUri = `${window.location.origin}/apps/oauth/callback`;
  const verifier = randomUrlSafe(48);
  const state = randomUrlSafe(16);
  const challenge = await codeChallenge(verifier);
  const scope = ["openid", ...scopes].join(" ");

  const authUrl =
    `${issuer}/protocol/openid-connect/auth` +
    `?client_id=${encodeURIComponent(clientIdB1)}` +
    `&response_type=code&redirect_uri=${encodeURIComponent(redirectUri)}` +
    `&scope=${encodeURIComponent(scope)}` +
    `&state=${encodeURIComponent(state)}` +
    `&code_challenge=${encodeURIComponent(challenge)}&code_challenge_method=S256`;

  const popup = window.open(authUrl, "shiki-miniapp-oauth", "width=480,height=640");
  if (!popup) throw new Error("ポップアップがブロックされました。許可して再試行してください");

  const code = await new Promise<string>((resolve, reject) => {
    const timer = window.setInterval(() => {
      if (popup.closed) {
        window.clearInterval(timer);
        window.removeEventListener("message", onMessage);
        reject(new Error("認可がキャンセルされました"));
      }
    }, 500);
    function onMessage(ev: MessageEvent) {
      // callback ページは同一オリジン。それ以外からのメッセージは無視（混線防止）。
      if (ev.origin !== window.location.origin) return;
      const data = ev.data as { type?: string; code?: string; state?: string };
      if (data?.type !== "shiki:oauth-code") return;
      window.clearInterval(timer);
      window.removeEventListener("message", onMessage);
      if (data.state !== state) {
        reject(new Error("state が一致しません（認可応答の混線）"));
        return;
      }
      if (!data.code) {
        reject(new Error("認可コードがありません"));
        return;
      }
      resolve(data.code);
    }
    window.addEventListener("message", onMessage);
  });

  // token 交換（public client + PKCE・client webOrigins による CORS 前提）。
  const res = await fetch(`${issuer}/protocol/openid-connect/token`, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "authorization_code",
      client_id: clientIdB1,
      code,
      redirect_uri: redirectUri,
      code_verifier: verifier,
    }),
  });
  if (!res.ok) throw new Error(`トークン交換に失敗しました（HTTP ${res.status}）`);
  const body = (await res.json()) as { access_token: string; expires_in: number };
  return { accessToken: body.access_token, expiresIn: body.expires_in };
}
