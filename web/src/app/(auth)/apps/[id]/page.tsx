"use client";

/// ミニアプリ実行画面（Task 6.11・#604）: 画面（Yui の ```ui を含む Markdown）を描き、図のボタンで
/// 束ねたワークフローを起動する。起動の記録（本人の分）と、ワークフローが画面に返した結果（GAP-09）は
/// 図の下に出し、終わるまで読み直す（待機中だけなら間隔を空ける・BUG-20）。起動が終わったら画面の表を
/// 読み直す（BUG-19）。バージョン切替は「その版を解決して描画」＝再現性の確認にも使える。
/// 束ねたスキルが使えない（削除された・本文が壊れている）ときもアプリは開き、見出しの下に理由を出す（BUG-17）。

import * as React from "react";
import { use } from "react";
import { AppWindow, History, Loader2, Pencil, Share2, TriangleAlert } from "lucide-react";
import Link from "next/link";

import { ChromeIconButton, usePageChrome } from "@/components/shell/page-header-context";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { toast } from "@/components/ui/use-toast";
import { ArtifactShareDialog } from "@/components/artifacts/share-dialog";
import { MiniAppEditorDialog } from "@/components/artifacts/miniapp-editor";
import { VersionsDialog } from "@/components/artifacts/versions-dialog";
import { DataGridHostContext, miniAppDataGridHost } from "@/components/data/data-grid-host";
import { Markdown } from "@/components/prompt-kit/markdown";
import { useRunsRefresh, YuiActionContext, type YuiActionHost } from "@/components/prompt-kit/yui-actions";
import { useMe } from "@/hooks/use-me";
import {
  getArtifact,
  getMiniAppRun,
  invokeMiniAppYuiAction,
  listMiniAppRuns,
  resolveMiniApp,
  type ArtifactMeta,
  type MiniAppRun,
  type ResolvedMiniApp,
} from "@/lib/artifact-api";

/// 起動の状態のうち、まだ終わっていないもの（起動中・順番待ち・実行中・待機中）。
const OPEN_RUN_STATUSES: ReadonlySet<MiniAppRun["status"]> = new Set(["starting", "queued", "running", "waiting"]);

/// 前の一覧から終わった起動があるか（BUG-19）。前の一覧で動いていたものが終わったか、前の一覧に無かったものが
/// 終わった状態で現れたら真。`prev` が null（最初の一覧）なら基準にするだけ。
///
/// 起動 1 件の鍵はボタンと押した時刻（run_id は起動中には無く、付いたときに鍵が変わってしまう）。戻り値の
/// `seen` は鍵 → まだ動いているか。
function runsFinishedSince(
  prev: Map<string, boolean> | null,
  runs: MiniAppRun[],
): { seen: Map<string, boolean>; finished: boolean } {
  const seen = new Map<string, boolean>();
  let finished = false;
  for (const r of runs) {
    const key = `${r.action_id}-${r.invoked_at}`;
    const open = OPEN_RUN_STATUSES.has(r.status);
    seen.set(key, open);
    if (prev && !open && prev.get(key) !== false) finished = true;
  }
  return { seen, finished };
}

export default function MiniAppRunPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = use(params);
  const me = useMe();
  const [app, setApp] = React.useState<ResolvedMiniApp | null>(null);
  const [meta, setMeta] = React.useState<ArtifactMeta | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  /// 見つからない・見る権限がない（他のテナントのアプリを含む・存在を区別しない）。
  const [notFound, setNotFound] = React.useState(false);
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
    setNotFound(false);
    Promise.all([resolveMiniApp(id, version), getArtifact(id)])
      .then(([resolved, m]) => {
        if (!active) return;
        setApp(resolved);
        setMeta(m);
      })
      .catch((e) => {
        if (!active) return;
        const status = (e as { status?: number }).status;
        if (status === 403 || status === 404) setNotFound(true);
        else setError(e instanceof Error ? e.message : "読み込みに失敗しました");
      });
    return () => {
      active = false;
    };
  }, [id, version, reloadKey]);

  // 起動の記録（本人の分）。押した後と、終わっていない起動がある間は読み直す。
  const [runsKey, setRunsKey] = React.useState(0);
  // 読み直しを終えた回数（失敗も数える）。読めなかったときも次の読み直しを予約するため。
  const [runsLoaded, setRunsLoaded] = React.useState(0);
  const refreshRuns = React.useCallback(() => setRunsKey((k) => k + 1), []);
  React.useEffect(() => {
    let active = true;
    listMiniAppRuns(id)
      .then((r) => active && setRuns(r))
      .catch(() => {
        /* 記録が読めなくても画面は使える（終わっていない起動があれば下で読み直す） */
      })
      .finally(() => active && setRunsLoaded((n) => n + 1));
    return () => {
      active = false;
    };
  }, [id, runsKey]);
  useRunsRefresh(runs, refreshRuns, runsLoaded);

  // 起動が終わったら画面の表（```data_grid）を読み直す（ワークフローが書いた行を出す・BUG-19）。
  // 前に読んだときに動いていた起動が終わった状態になったとき、または前の一覧に無かった起動がいきなり終わった
  // 状態で現れたとき（短いフローは、押した直後の最初の一覧でもう終わっている）。最初の一覧は基準にするだけ。
  const [gridRevision, setGridRevision] = React.useState(0);
  const seenRuns = React.useRef<Map<string, boolean> | null>(null);
  React.useEffect(() => {
    if (runsLoaded === 0) return;
    const { seen, finished } = runsFinishedSince(seenRuns.current, runs);
    seenRuns.current = seen;
    if (finished) setGridRevision((n) => n + 1);
  }, [runs, runsLoaded]);

  // 使えない部品（壊れた任意のピン）のうちスキルの分。画面とボタンはそのまま使える（BUG-17）。
  const skillUnavailable = app?.unavailable.find((u) => u.path === "skill");

  // 編集の対象は読み込んだ版が変わったときだけ作り直す（記録の読み直しで編集中の入力を戻さない）。
  const editTarget = React.useMemo(
    () => (app ? { id: app.id, version: app.version, body: app.body } : undefined),
    [app],
  );

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
            // 結果が付いていない古い記録は、開いたときに 1 件だけ読む（本人の分だけ返る・GAP-09）。
            loadRun: (runId) => getMiniAppRun(app.id, runId),
          }
        : null,
    [app, runs, refreshRuns],
  );

  // 画面の表（```data_grid）は束ねた保存ビューを閲覧者本人の権限でページごとに実行する（Task 15.5）。
  // 起動が終わるたびに作り直し、表を読み直させる（BUG-19）。
  const gridHost = React.useMemo(
    () => (app ? miniAppDataGridHost(app.id, app.version, gridRevision) : null),
    [app, gridRevision],
  );

  return (
    <div className="mx-auto w-full max-w-3xl px-4 pb-6 pt-1 md:py-6">
      {notFound ? (
        <EmptyState
          icon={AppWindow}
          title="アプリが見つかりません"
          description="削除されたか、このアプリを使う権限がありません。共有した人に確かめてください。"
          action={
            <Button variant="outline" asChild>
              <Link href="/apps">アプリ一覧へ</Link>
            </Button>
          }
        />
      ) : error ? (
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
              {app.body.skill ? (skillUnavailable ? "・スキルは使えません" : "・スキル適用") : ""}
              {app.body.workflows.length > 0 ? `・ワークフロー ${app.body.workflows.length} 件` : ""}
            </p>
            {skillUnavailable ? (
              <p
                role="note"
                data-testid="miniapp-skill-unavailable"
                className="mt-3 flex items-start gap-2 rounded-lg border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-sm text-amber-700 dark:text-amber-400"
              >
                <TriangleAlert className="mt-0.5 size-4 shrink-0" aria-hidden />
                <span className="min-w-0 [overflow-wrap:anywhere]">
                  {skillUnavailable.message}。このアプリのスキルだけが使えません。画面とボタンはそのまま使えます。
                </span>
              </p>
            ) : null}
          </div>

          <YuiActionContext.Provider value={host}>
            <DataGridHostContext.Provider value={gridHost}>
              <Markdown key={`${app.id}-v${app.version}`}>{app.body.screen}</Markdown>
            </DataGridHostContext.Provider>
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
              app={editTarget}
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
