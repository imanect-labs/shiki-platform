"use client";

import { UserManager, WebStorageStateStore } from "oidc-client-ts";

let manager: UserManager | null = null;

/// Keycloak (OIDC public client + PKCE) 用の UserManager をブラウザ側で生成する。
export function getUserManager(): UserManager {
  if (manager) return manager;
  const authority =
    process.env.NEXT_PUBLIC_OIDC_AUTHORITY ?? "http://localhost:8081/realms/shiki";
  const clientId = process.env.NEXT_PUBLIC_OIDC_CLIENT_ID ?? "shiki-web";
  const origin = window.location.origin;

  manager = new UserManager({
    authority,
    client_id: clientId,
    redirect_uri: `${origin}/callback`,
    post_logout_redirect_uri: origin,
    response_type: "code",
    scope: "openid profile",
    // トークンはローカルストレージに保持（リフレッシュ含む）。
    userStore: new WebStorageStateStore({ store: window.localStorage }),
    automaticSilentRenew: true,
  });
  return manager;
}
