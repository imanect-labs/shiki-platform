"use client";

/// ミニアプリ実行画面（Task 6.11・#604）: 画面（Yui の ```ui を含む Markdown）を描き、図のボタンで
/// 束ねたワークフローを起動する。起動の記録（本人の分）は図の下に出し、終わるまで読み直す。
/// バージョン切替は「その版を解決して描画」＝再現性の確認にも使える。

import * as React from "react";
import { use } from "react";
import { History, Loader2, Pencil, Share2 } from "lucide-react";

import { ChromeIconButton, usePageChrome } from "@/components/shell/page-header-context";
import { toast } from "@/components/ui/use-toast";
import { ArtifactShareDialog } from "@/components/artifacts/share-dialog";
import { MiniAppEditorDialog } from "@/components/artifacts/miniapp-editor";
import { VersionsDialog } from "@/components/artifacts/versions-dialog";
import { Markdown } from "@/components/prompt-kit/markdown";
import { hasOpenRuns, YuiActionContext, type YuiActionHost } from "@/components/prompt-kit/yui-actions";
import { useMe } from "@/hooks/use-me";
import {
  getArtifact,
  invokeMiniAppYuiAction,
  listMiniAppRuns,
  resolveMiniApp,
  type ArtifactMeta,
  type MiniAppRun,
  type ResolvedMiniApp,
} from "@/lib/artifact-api";

export default function MiniAppRunPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = use(params);
  const me = useMe();
  const [app, setApp] = React.useState<ResolvedMiniApp | null>(null);
  const [meta, setMeta] = React.useState<ArtifactMeta | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [version, setVersion] = React.useState<number | undefined>(undefined);
  const [reloadKey, setReloadKey] = React.useState(0);
  const [runs, setRuns] = React.useState<MiniAppRun[]>([]);
  const [shareOpen, setShareOpen] = React.useState(false);
  const [versionsOpen, setVersionsOpen] = React.useState(false);
  const [editOpen, setEditOpen] = React.useState(false);
  // 編集は持ち主だけ（共有された人は使うだけ）。最新の版を見ているときに限る。
  const canEdit = meta != null && me.data?.id === meta.owner && app?.version === meta.currentVersion;

  // 版・共有・編集はシェルのクローム（PC はバー右・スマホは右上）に置く。
  const appName = app?.body.description;
  usePageChrome(
    () => ({
      title: appName,
      subtitle: app ? `v${app.version}` : undefined,
      back: { href: "/apps", label: "アプリ一覧へ" },
      actions: app ? (
        <>
          {canEdit ? (
            <ChromeIconButton label="編集" onClick={() => setEditOpen(true)}>
              <Pencil aria-hidden />
            </ChromeIconButton>
          ) : null}
          <ChromeIconButton label="バージョン" onClick={() => setVersionsOpen(true)}>
            <History aria-hidden />
          </ChromeIconButton>
          <ChromeIconButton label="共有" onClick={() => setShareOpen(true)}>
            <Share2 aria-hidden />
          </ChromeIconButton>
        </>
      ) : undefined,
    }),
    [appName, app?.version, Boolean(app), canEdit],
  );

  React.useEffect(() => {
    let active = true;
    setError(null);
    Promise.all([resolveMiniApp(id, version), getArtifact(id)])
      .then(([resolved, m]) => {
        if (!active) return;
        setApp(resolved);
        setMeta(m);
      })
      .catch((e) => active && setError(e instanceof Error ? e.message : "読み込みに失敗しました"));
    return () => {
      active = false;
    };
  }, [id, version, reloadKey]);

  // 起動の記録（本人の分）。押した後と、終わっていない起動がある間は読み直す。
  const [runsKey, setRunsKey] = React.useState(0);
  const refreshRuns = React.useCallback(() => setRunsKey((k) => k + 1), []);
  React.useEffect(() => {
    let active = true;
    listMiniAppRuns(id)
      .then((r) => active && setRuns(r))
      .catch(() => {
        /* 記録が読めなくても画面は使える */
      });
    return () => {
      active = false;
    };
  }, [id, runsKey]);
  const openRuns = hasOpenRuns(runs);
  React.useEffect(() => {
    if (!openRuns) return;
    const t = window.setTimeout(refreshRuns, 3000);
    return () => window.clearTimeout(t);
  }, [openRuns, runs, refreshRuns]);

  const host = React.useMemo<YuiActionHost | null>(
    () =>
      app
        ? {
            // ミニアプリのボタンは必ずワークフローを起動する（会話に送るボタンは保存時に弾かれる）。
            invoke: (key, values, idempotencyKey) =>
              invokeMiniAppYuiAction(app.id, app.version, key, values, idempotencyKey ?? crypto.randomUUID()),
            // 実行先は保存時に束ねたワークフローに限られる（サーバーが確かめ済み）。
            isBound: () => true,
            invokedActions: [],
            // 押した版の記録だけを、その版の図の下に出す（版で画面が違うと識別子も違う）。
            uiRuns: runs.filter((r) => r.app_version === app.version),
            onChanged: refreshRuns,
          }
        : null,
    [app, runs, refreshRuns],
  );

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

          <YuiActionContext.Provider value={host}>
            <Markdown key={`${app.id}-v${app.version}`}>{app.body.screen}</Markdown>
          </YuiActionContext.Provider>

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
          {canEdit ? (
            <MiniAppEditorDialog
              open={editOpen}
              onOpenChange={setEditOpen}
              app={{ id: app.id, version: app.version, body: app.body }}
              onSaved={(saved) => {
                toast({ title: `v${saved.version} として保存しました` });
                setVersion(undefined);
                setReloadKey((k) => k + 1);
              }}
            />
          ) : null}
        </>
      )}
    </div>
  );
}
