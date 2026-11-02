"use client";

/// ミニアプリ実行画面（Task 6.11・#604）: 画面（Yui の ```ui を含む Markdown）を描き、図のボタンで
/// 束ねたワークフローを起動する。起動の記録（本人の分）と、ワークフローが画面に返した結果（GAP-09）は
/// 図の下に出し、終わるまで読み直す（待機中だけなら間隔を空ける・BUG-20）。起動が終わったら画面の表を
/// 読み直す（BUG-19）。
/// 束ねたスキルが使えない（削除された・本文が壊れている）ときもアプリは開き、見出しの下に理由を出す（BUG-17）。
///
/// 下書きと公開（D-16・GAP-36）: 共有した人（使う人）が開くのは公開したバージョンだけ。作る人（editor）は公開の帯で
/// 下書きを試し、公開する・公開を戻す・前のバージョンの内容を下書きにする。バージョン履歴も作る人にだけ出す。

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
import {
  MiniAppPublishBar,
  PublishMiniAppDialog,
  publishState,
  UNPUBLISHED_SHARE_NOTE,
} from "@/components/miniapp/publish";
import { DataGridHostContext, miniAppDataGridHost } from "@/components/data/data-grid-host";
import { Markdown } from "@/components/prompt-kit/markdown";
import { useRunsRefresh, YuiActionContext, type YuiActionHost } from "@/components/prompt-kit/yui-actions";
import {
  errorCode,
  getMiniAppRun,
  invokeMiniAppYuiAction,
  listMiniAppRuns,
  resolveMiniApp,
  updateMiniApp,
  type MiniAppRun,
  type MiniAppVersion,
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

/// 保存した下書きの知らせ（次に起きることと、公開を止める書き損じ）。
function savedToast(saved: MiniAppVersion): { title: string; description: string } {
  const title = `v${saved.version} を下書きとして保存しました`;
  if (saved.warnings.length > 0) {
    return { title, description: `画面に直す箇所が ${saved.warnings.length} 件あります。直すまで公開できません。` };
  }
  return {
    title,
    description:
      saved.published_version == null
        ? "公開すると、共有した人が使えるようになります。"
        : `公開するまで、共有した人には v${saved.published_version} が見えます。`,
  };
}

export default function MiniAppRunPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = use(params);
  const [app, setApp] = React.useState<ResolvedMiniApp | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  /// 見つからない・見る権限がない（他のテナントのアプリを含む・存在を区別しない）。
  const [notFound, setNotFound] = React.useState(false);
  /// 使う人が、まだ公開していないアプリを開いた（D-16）。
  const [notPublished, setNotPublished] = React.useState(false);
  /// 開くバージョン（`undefined` は公開しているバージョン。まだ公開していなければ作る人には最新）。
  const [version, setVersion] = React.useState<number | undefined>(undefined);
  const [reloadKey, setReloadKey] = React.useState(0);
  const [runs, setRuns] = React.useState<MiniAppRun[]>([]);
  const [shareOpen, setShareOpen] = React.useState(false);
  const [versionsOpen, setVersionsOpen] = React.useState(false);
  const [editTarget, setEditTarget] = React.useState<{ id: string; version: number; body: ResolvedMiniApp["body"] } | null>(
    null,
  );
  const [publishing, setPublishing] = React.useState<number | null>(null);
  const [restoring, setRestoring] = React.useState(false);
  // 編集・公開・バージョン履歴はアプリの editor 以上（持ち主と、共有で「編集」にした人・BUG-18）。閲覧の共有は使うだけで、
  // 公開したバージョンだけを開く（D-16）。編集できるかはサーバが返す（保存・公開は API が改めて確かめる）。
  const canEdit = app?.can_edit === true;
  // 共有（相手の一覧と付与・解除）はアプリの owner だけができる。出すかはサーバの答え（`can_share`）で決め、
  // 閲覧・編集の共有を受けた人には出さない（一覧が空に見え、付与が 403 になる・BUG-78）。
  const canShare = app?.can_share === true;
  const latest = app?.latest_version ?? app?.version;

  /// 編集は最新のバージョン（下書き）から始める。いま開いているのが別のバージョンなら、最新を読んでから開く。
  const openEditor = React.useCallback(async () => {
    if (!app) return;
    if (latest == null || app.version === latest) {
      setEditTarget({ id: app.id, version: app.version, body: app.body });
      return;
    }
    try {
      const head = await resolveMiniApp(app.id, latest);
      setEditTarget({ id: head.id, version: head.version, body: head.body });
    } catch (e) {
      toast({
        variant: "destructive",
        title: "最新のバージョンを読み込めませんでした",
        description: e instanceof Error ? e.message : String(e),
      });
    }
  }, [app, latest]);

  // 版・共有・編集はシェルのクローム（PC はバー右・スマホは右上）に置く。
  const appName = app?.body.description;
  const subtitle = app ? (canEdit && publishState(app) === "draft" ? `v${app.version}（下書き）` : `v${app.version}`) : undefined;
  usePageChrome(
    () => ({
      title: appName,
      subtitle,
      back: { href: "/apps", label: "アプリ一覧へ" },
      // 使う人（閲覧の共有）には右上の操作が無い。空の枠を出さない。
      actions: app && (canEdit || canShare) ? (
        <>
          {canEdit ? (
            <ChromeIconButton label="編集" onClick={() => void openEditor()}>
              <Pencil aria-hidden />
            </ChromeIconButton>
          ) : null}
          {canEdit ? (
            <ChromeIconButton label="バージョン" onClick={() => setVersionsOpen(true)}>
              <History aria-hidden />
            </ChromeIconButton>
          ) : null}
          {canShare ? (
            <ChromeIconButton label="共有" onClick={() => setShareOpen(true)}>
              <Share2 aria-hidden />
            </ChromeIconButton>
          ) : null}
        </>
      ) : undefined,
    }),
    [appName, subtitle, Boolean(app), canEdit, canShare, openEditor],
  );

  React.useEffect(() => {
    let active = true;
    setError(null);
    setNotFound(false);
    setNotPublished(false);
    resolveMiniApp(id, version)
      .then((resolved) => {
        if (!active) return;
        setApp(resolved);
      })
      .catch((e) => {
        if (!active) return;
        const status = (e as { status?: number }).status;
        const code = errorCode(e);
        if (code === "not_published") setNotPublished(true);
        else if (code === "app_version_changed") setVersion(undefined);
        else if (status === 403 || status === 404) setNotFound(true);
        else setError(e instanceof Error ? e.message : "読み込みに失敗しました");
      });
    return () => {
      active = false;
    };
  }, [id, version, reloadKey]);

  /// 開いているバージョンの内容で新しい下書きを保存する（前のバージョンに戻す・公開は変えない）。
  const restore = async (from: number) => {
    if (!app || latest == null) return;
    setRestoring(true);
    try {
      const saved = await updateMiniApp(app.id, app.body, latest);
      const t = savedToast(saved);
      toast({ title: `v${from} の内容で v${saved.version} を下書きとして保存しました`, description: t.description });
      setVersion(saved.version);
      setReloadKey((k) => k + 1);
    } catch (e) {
      toast({
        variant: "destructive",
        title: "下書きを保存できませんでした",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setRestoring(false);
    }
  };

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
      {notPublished ? (
        <EmptyState
          icon={AppWindow}
          title="このアプリはまだ公開されていません"
          description="作成した人が公開すると使えるようになります。"
          action={
            <Button variant="outline" asChild>
              <Link href="/apps">アプリ一覧へ</Link>
            </Button>
          }
        />
      ) : notFound ? (
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
            {canEdit ? (
              <MiniAppPublishBar
                app={app}
                onShowVersion={setVersion}
                onPublish={setPublishing}
                onRestore={(v) => void restore(v)}
                restoring={restoring}
              />
            ) : null}
          </div>

          <YuiActionContext.Provider value={host}>
            <DataGridHostContext.Provider value={gridHost}>
              <Markdown key={`${app.id}-v${app.version}`}>{app.body.screen}</Markdown>
            </DataGridHostContext.Provider>
          </YuiActionContext.Provider>

          {canShare ? (
            <ArtifactShareDialog
              open={shareOpen}
              onOpenChange={setShareOpen}
              artifactId={app.id}
              name={app.body.description}
              note={app.published_version == null ? UNPUBLISHED_SHARE_NOTE : undefined}
              shareUrl={
                typeof window !== "undefined"
                  ? `${window.location.origin}/apps/${app.id}`
                  : undefined
              }
            />
          ) : null}
          {canEdit ? (
            <>
              <VersionsDialog
                open={versionsOpen}
                onOpenChange={setVersionsOpen}
                artifactId={app.id}
                name={app.body.description}
                currentVersion={app.version}
                onSelectVersion={(v) => setVersion(v)}
                badges={(v) => [
                  ...(v === app.published_version ? ["公開中"] : []),
                  ...(v === app.previous_published_version ? ["前に公開"] : []),
                  ...(v === latest && v !== app.published_version ? ["下書き"] : []),
                ]}
              />
              <MiniAppEditorDialog
                open={editTarget != null}
                onOpenChange={(open) => !open && setEditTarget(null)}
                app={editTarget ?? undefined}
                onSaved={(saved) => {
                  toast(savedToast(saved));
                  // 保存した下書きを開いて試せるようにする（使う人の画面は公開するまで変わらない）。
                  setVersion(saved.version);
                  setReloadKey((k) => k + 1);
                }}
              />
              {publishing != null ? (
                <PublishMiniAppDialog
                  open
                  onOpenChange={(open) => !open && setPublishing(null)}
                  appId={app.id}
                  version={publishing}
                  publishedVersion={app.published_version ?? null}
                  onPublished={(p) => {
                    toast({
                      title: `v${p.published_version} を公開しました`,
                      description: `共有した人の画面が v${p.published_version} に変わりました。`,
                    });
                    setVersion(undefined);
                    setReloadKey((k) => k + 1);
                  }}
                />
              ) : null}
            </>
          ) : null}
        </>
      )}
    </div>
  );
}
