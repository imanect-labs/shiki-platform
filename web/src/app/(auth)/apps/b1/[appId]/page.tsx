"use client";

/// B1 コードミニアプリの実行画面（Task 9.11）。
///
/// インストール情報（同意時ピン）を解決して [`MiniAppFrame`] で起動する。
/// 宣言的アプリ（/apps/[id]）と同じ「アプリ」一覧から到達する＝A/B 同一シェル。

import * as React from "react";
import { use } from "react";

import { MiniAppFrame, MiniAppFrameFallback } from "@/components/miniapp/miniapp-frame";
import { listInstallations, type AppInstallation } from "@/lib/miniapp-b1-api";

export default function MiniAppB1Page({ params }: { params: Promise<{ appId: string }> }) {
  const { appId } = use(params);
  const [app, setApp] = React.useState<AppInstallation | null | undefined>(undefined);

  React.useEffect(() => {
    let active = true;
    listInstallations()
      .then((items) => {
        if (!active) return;
        setApp(items.find((i) => i.app_id === appId) ?? null);
      })
      .catch(() => active && setApp(null));
    return () => {
      active = false;
    };
  }, [appId]);

  return (
    <div className="mx-auto flex h-full w-full max-w-5xl flex-col px-4 py-6">
      {app === undefined ? (
        <MiniAppFrameFallback />
      ) : app === null ? (
        <div className="rounded-lg border border-destructive/30 bg-destructive/5 px-4 py-6 text-center text-sm text-destructive">
          このアプリはインストールされていません。
        </div>
      ) : (
        <>
          <div className="mb-3 min-w-0">
            <h1 className="truncate text-lg font-semibold">{app.app_name}</h1>
            <p className="text-xs text-muted-foreground">
              v{app.installed_version}・sandboxed（ゲートウェイ経由のみ通信可能）
            </p>
          </div>
          <MiniAppFrame installation={app} />
        </>
      )}
    </div>
  );
}
