"use client";

/// ミニアプリ実行画面（Task 6.11）: 解決済み UI スペックを描画し、宣言済みアクションを実行する。
/// バージョン切替は「その版を解決して描画」＝再現性の確認にも使える。

import * as React from "react";
import { use } from "react";
import { History, Loader2, Share2 } from "lucide-react";

import { ChromeIconButton, usePageChrome } from "@/components/shell/page-header-context";
import { toast } from "@/components/ui/use-toast";
import { ArtifactShareDialog } from "@/components/artifacts/share-dialog";
import { VersionsDialog } from "@/components/artifacts/versions-dialog";
import { MiniAppGenUiProvider } from "@/components/genui/action-context";
import { SpecRenderer } from "@/components/genui/spec-renderer";
import { resolveMiniApp, type ResolvedMiniApp } from "@/lib/artifact-api";

export default function MiniAppRunPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = use(params);
  const [app, setApp] = React.useState<ResolvedMiniApp | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [version, setVersion] = React.useState<number | undefined>(undefined);
  const [shareOpen, setShareOpen] = React.useState(false);
  const [versionsOpen, setVersionsOpen] = React.useState(false);

  // 版・共有はシェルのクローム（PC はバー右・スマホは右上）に置く。
  const appName = app?.body.description;
  usePageChrome(
    () => ({
      title: appName,
      subtitle: app ? `v${app.version}` : undefined,
      back: { href: "/apps", label: "アプリ一覧へ" },
      actions: app ? (
        <>
          <ChromeIconButton label="バージョン" onClick={() => setVersionsOpen(true)}>
            <History aria-hidden />
          </ChromeIconButton>
          <ChromeIconButton label="共有" onClick={() => setShareOpen(true)}>
            <Share2 aria-hidden />
          </ChromeIconButton>
        </>
      ) : undefined,
    }),
    [appName, app?.version, Boolean(app)],
  );

  React.useEffect(() => {
    let active = true;
    setApp(null);
    setError(null);
    resolveMiniApp(id, version)
      .then((resolved) => active && setApp(resolved))
      .catch((e) => active && setError(e instanceof Error ? e.message : "読み込みに失敗しました"));
    return () => {
      active = false;
    };
  }, [id, version]);

  return (
    <div className="mx-auto w-full max-w-3xl px-4 pb-6 pt-1 md:py-6">
      {error ? (
        <div className="rounded-lg border border-destructive/30 bg-destructive/5 px-4 py-6 text-center text-sm text-destructive">
          {error}
        </div>
      ) : app === null ? (
        <div className="flex items-center justify-center gap-2 py-16 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" aria-hidden />
          読み込み中…
        </div>
      ) : (
        <>
          <div className="mb-4 min-w-0">
            <h1 className="text-[26px] font-bold leading-tight tracking-tight md:text-lg md:font-semibold">
              {app.body.description}
            </h1>
            <p className="mt-0.5 text-xs text-muted-foreground">
              v{app.version}
              {app.body.skill ? "・スキル適用" : ""}
              {app.body.workflows.length > 0 ? `・ワークフロー ${app.body.workflows.length} 件` : ""}
            </p>
          </div>

          <MiniAppGenUiProvider
            appId={app.id}
            version={app.version}
            onActionCompleted={(result) => {
              if (result?.result.kind === "workflow") {
                toast({ title: "ワークフローを起動しました" });
              }
            }}
          >
            <SpecRenderer key={`${app.id}-v${app.version}`} spec={app.ui_spec} className="my-0" />
          </MiniAppGenUiProvider>

          <ArtifactShareDialog
            open={shareOpen}
            onOpenChange={setShareOpen}
            artifactId={app.id}
            name={app.body.description}
            shareUrl={
              typeof window !== "undefined"
                ? `${window.location.origin}/apps/${app.id}`
                : undefined
            }
          />
          <VersionsDialog
            open={versionsOpen}
            onOpenChange={setVersionsOpen}
            artifactId={app.id}
            name={app.body.description}
            currentVersion={app.version}
            onSelectVersion={(v) => setVersion(v)}
          />
        </>
      )}
    </div>
  );
}
