"use client";

/// B1 コードミニアプリの実行フレーム（Task 9.11）。
///
/// 隔離モデル:
/// - `sandbox="allow-scripts allow-forms"`（**allow-same-origin なし** = opaque origin）。
///   配信側 CSP の `sandbox` ディレクティブと二重で効く。アプリはホストの DOM/Cookie/
///   storage に構造的に到達できない。
/// - 通信は配信側 CSP `connect-src` でゲートウェイのみに限定。
/// - トークンは **ホスト支援 PKCE**: アプリが `shiki:token-request` を postMessage →
///   シェルが popup で取得し `shiki:token` を返す。opaque origin のため targetOrigin は
///   `"*"` を使わざるを得ないが、**iframe の contentWindow へ直接送る**ため第三者へは
///   渡らない（source 検証も行う）。

import * as React from "react";
import { Loader2 } from "lucide-react";

import { acquireGatewayToken } from "@/lib/miniapp-oauth";
import { b1Origin, gatewayOrigin, type AppInstallation } from "@/lib/miniapp-b1-api";

export function MiniAppFrame({ installation }: { installation: AppInstallation }) {
  const frameRef = React.useRef<HTMLIFrameElement | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const src = installation.frontend_bundle
    ? `${b1Origin()}/a/${installation.app_id}/${installation.frontend_bundle}`
    : null;

  React.useEffect(() => {
    if (!src) return;
    const clientId = installation.client_id_b1;
    function onMessage(ev: MessageEvent) {
      // トークン要求はこの iframe からのみ受理（opaque origin ⇒ ev.origin は "null"）。
      const frameWindow = frameRef.current?.contentWindow;
      if (!frameWindow || ev.source !== frameWindow) return;
      const data = ev.data as { type?: string; scopes?: string[] };
      if (data?.type !== "shiki:token-request") return;
      if (!clientId) {
        frameWindow.postMessage(
          { type: "shiki:token-error", error: "B1 client が未登録です" },
          "*",
        );
        return;
      }
      const scopes = Array.isArray(data.scopes) ? data.scopes : installation.granted_scopes;
      acquireGatewayToken(clientId, scopes)
        .then((token) =>
          frameWindow.postMessage(
            {
              type: "shiki:token",
              accessToken: token.accessToken,
              expiresIn: token.expiresIn,
              gatewayOrigin: gatewayOrigin(),
            },
            "*",
          ),
        )
        .catch((e) =>
          frameWindow.postMessage(
            { type: "shiki:token-error", error: e instanceof Error ? e.message : "認可失敗" },
            "*",
          ),
        );
    }
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [src, installation]);

  if (!src) {
    return (
      <div className="rounded-lg border border-dashed px-4 py-10 text-center text-sm text-muted-foreground">
        このアプリにはフロントバンドルがありません（B2 のみ）。
      </div>
    );
  }
  return (
    <div className="flex h-full min-h-[480px] flex-col">
      {error ? (
        <div className="mb-2 rounded border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-destructive">
          {error}
        </div>
      ) : null}
      <iframe
        ref={frameRef}
        src={src}
        title={installation.app_name}
        // allow-same-origin を**付けない**（opaque origin 化・ホスト隔離の要）。
        sandbox="allow-scripts allow-forms"
        className="min-h-[480px] w-full flex-1 rounded-lg border bg-background"
        onError={() => setError("バンドルの読み込みに失敗しました")}
      />
    </div>
  );
}

export function MiniAppFrameFallback() {
  return (
    <div className="flex items-center justify-center gap-2 py-16 text-sm text-muted-foreground">
      <Loader2 className="size-4 animate-spin" aria-hidden />
      読み込み中…
    </div>
  );
}
