"use client";

/// ミニアプリ実行画面（Task 6.11）: 解決済み UI スペックを描画し、宣言済みアクションを実行する。
/// バージョン切替は「その版を解決して描画」＝再現性の確認にも使える。

import * as React from "react";
import { use } from "react";
import { History, Loader2, Share2 } from "lucide-react";

import { Button } from "@/components/ui/button";
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
    <div className="mx-auto w-full max-w-3xl px-4 py-6">
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
          <div className="mb-4 flex items-start justify-between gap-3">
            <div className="min-w-0">
              <h1 className="text-lg font-semibold">{app.body.description}</h1>
              <p className="text-xs text-muted-foreground">
                v{app.version}
                {app.body.skill ? "・スキル適用" : ""}
                {app.body.workflows.length > 0 ? `・ワークフロー ${app.body.workflows.length} 件` : ""}
              </p>
            </div>
            <div className="flex shrink-0 items-center gap-1">
              <Button size="sm" variant="ghost" onClick={() => setVersionsOpen(true)}>
                <History className="size-4" aria-hidden />
                バージョン
              </Button>
              <Button size="sm" variant="ghost" onClick={() => setShareOpen(true)}>
                <Share2 className="size-4" aria-hidden />
                共有
              </Button>
            </div>
          </div>

          <MiniAppGenUiProvider
            appId={app.id}
            version={app.version}
            onActionCompleted={(result) => {
              if (result.result.kind === "workflow") {
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
