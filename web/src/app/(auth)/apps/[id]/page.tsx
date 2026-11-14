"use client";

/// ミニアプリ実行画面（Task 6.11・#604）: 画面（Yui の ```ui を含む Markdown）を描き、図のボタンで
/// 束ねたワークフローを起動する。起動の記録（本人の分）と、ワークフローが画面に返した結果（GAP-09）は
/// 図の下に出し、終わるまで読み直す（待機中だけなら間隔を空ける・BUG-20）。起動が終わったら画面の表を
/// 読み直す（BUG-19）。
/// 束ねたスキルが使えない（削除された・本文が壊れている）ときもアプリは開き、見出しの下に理由を出す（BUG-17）。
///
/// 下書きと公開（D-16・GAP-36）: 共有した人（使う人）が開くのは公開したバージョンだけ。作る人（editor）は公開の帯で
/// 下書きを試し、公開する・公開を戻す・前のバージョンの内容を下書きにする。バージョン履歴も作る人にだけ出す。
/// スキルを束ねたアプリには「このアプリについて AI に聞く」を置き、アプリを付けた会話を始める（D-16 の (b)）。
///
/// 「AI に依頼」パネル（D-10 T11）: 編集できる人には、文書のアシスタントと同じ形（PC は右に浮かぶカード・スマホは下からの
/// シート）で、このアプリを対象にした会話を出す。AI がこのアプリを保存したら、その下書きを開き直す。編集のダイアログを
/// 開いていればダイアログの中身はそのまま残し（人の書きかけを黙って捨てない）、保存すると版の照合（409）が最新の読み込みを
/// 促す。AI が保存ビューを直したら画面の表を読み直す。

import * as React from "react";
import { use } from "react";
import { AppWindow, History, Loader2, MessageSquare, Pencil, Share2, TriangleAlert } from "lucide-react";
import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";

import { ChromeIconButton, usePageChrome } from "@/components/shell/page-header-context";
import { AssistantLayout } from "@/components/shell/assistant-sheet";
import { MiniAppChatPanel } from "@/components/miniapp/app-assistant";
import { useDocThreadDeepLink } from "@/hooks/use-doc-thread-deep-link";
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
  savedToast,
  UNPUBLISHED_SHARE_NOTE,
} from "@/components/miniapp/publish";
import { DataGridHostContext, miniAppDataGridHost } from "@/components/data/data-grid-host";
import { RECHECK_ON_PRESS_MS, holdsBack, useMyAppAccess } from "@/components/miniapp/use-my-access";
import { Markdown } from "@/components/prompt-kit/markdown";
import { useRunsRefresh, YuiActionContext, type YuiActionHost } from "@/components/prompt-kit/yui-actions";
import {
  checkMiniAppAccess,
  errorCode,
  getMiniAppRun,
  invokeMiniAppYuiAction,
  isVersionConflict,
  listMiniAppRuns,
  resolveMiniApp,
  updateMiniApp,
  validationMessages,
  type MiniAppRun,
  type MyPartAccess,
  type ResolvedMiniApp,
} from "@/lib/artifact-api";
import { myBlockedReason } from "@/lib/access-wording";
import { createThread, type BuilderRef } from "@/lib/chat-api";

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

/// 部品（ボタンの実行先・画面の表）の、自分に足りない権限。
function findPart(
  parts: readonly MyPartAccess[] | undefined,
  kind: "workflow" | "data_view",
  alias: string,
): MyPartAccess | null {
  return parts?.find((p) => p.part.kind === kind && "alias" in p.part && p.part.alias === alias) ?? null;
}

export default function MiniAppRunPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = use(params);
  const router = useRouter();
  const searchParams = useSearchParams();
  const { chatOpen, setChatOpen, deepLinkThreadId } = useDocThreadDeepLink(searchParams.get("thread"));
  const [app, setApp] = React.useState<ResolvedMiniApp | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  /// 見つからない・見る権限がない（他のテナントのアプリを含む・存在を区別しない）。
  const [notFound, setNotFound] = React.useState(false);
  /// 使う人が、まだ公開していないアプリを開いた（D-16）。
  const [notPublished, setNotPublished] = React.useState(false);
  /// 開くバージョン（`undefined` は公開しているバージョン。まだ公開していなければ作る人には最新）。
  const [version, setVersion] = React.useState<number | undefined>(undefined);
  const [reloadKey, setReloadKey] = React.useState(0);
  // 画面の表を読み直した回数（起動が終わった・AI がビューを直した・権限が変わった）。
  const [gridRevision, setGridRevision] = React.useState(0);
  const [runs, setRuns] = React.useState<MiniAppRun[]>([]);
  const [shareOpen, setShareOpen] = React.useState(false);
  const [versionsOpen, setVersionsOpen] = React.useState(false);
  const [editTarget, setEditTarget] = React.useState<{ id: string; version: number; body: ResolvedMiniApp["body"] } | null>(
    null,
  );
  const [publishing, setPublishing] = React.useState<number | null>(null);
  const [restoring, setRestoring] = React.useState(false);
  const [asking, setAsking] = React.useState(false);
  // 編集・公開・バージョン履歴はアプリの editor 以上（持ち主と、共有で「編集」にした人・BUG-18）。閲覧の共有は使うだけで、
  // 公開したバージョンだけを開く（D-16）。編集できるかはサーバが返す（保存・公開は API が改めて確かめる）。
  const canEdit = app?.can_edit === true;
  // 共有（相手の一覧と付与・解除）はアプリの owner だけができる。出すかはサーバの答え（`can_share`）で決め、
  // 閲覧・編集の共有を受けた人には出さない（一覧が空に見え、付与が 403 になる・BUG-78）。
  const canShare = app?.can_share === true;
  const latest = app?.latest_version ?? app?.version;
  // 知っている最新の版（AI の保存の知らせを版で冪等に扱う・再訪の送り直しで同じ版を受けても読み直さない）。
  const latestRef = React.useRef<number | undefined>(undefined);
  latestRef.current = latest;
  const editingRef = React.useRef(false);
  editingRef.current = editTarget != null;

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

  /// ほかの人が先に保存したとき、最新のバージョン（下書き）を読み直してエディターに入れ直す（付録 C の §3.5）。
  const reloadLatestIntoEditor = React.useCallback(async () => {
    if (!app) return;
    try {
      // バージョンを省くと公開しているバージョンと、作る人には最新のバージョンの番号が返る。
      const now = await resolveMiniApp(app.id);
      const head =
        now.latest_version != null && now.latest_version !== now.version
          ? await resolveMiniApp(app.id, now.latest_version)
          : now;
      setEditTarget({ id: head.id, version: head.version, body: head.body });
      setReloadKey((k) => k + 1);
    } catch (e) {
      toast({
        variant: "destructive",
        title: "最新の内容を読み込めませんでした",
        description: e instanceof Error ? e.message : String(e),
      });
    }
  }, [app]);

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
      // 「AI に依頼」は直せる人にだけ出す（使う人に AI に直させる口を見せない）。
      assistant: canEdit
        ? { active: chatOpen, onToggle: () => setChatOpen(!chatOpen), testId: "miniapp-assistant-toggle" }
        : undefined,
    }),
    [appName, subtitle, Boolean(app), canEdit, canShare, openEditor, chatOpen, setChatOpen],
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

  /// AI がアプリ・保存ビューを保存したとき（パネルの会話の builder_ref・D-10 T11）。
  const onBuilderSaved = React.useCallback(
    (ref: BuilderRef) => {
      if (ref.kind === "data_view") {
        // 束ねたビューかは画面の表が知っている。読み直すだけなので、どのビューでも表を読み直す。
        setGridRevision((n) => n + 1);
        return;
      }
      if (ref.kind !== "mini_app" || ref.id !== id) return;
      if (latestRef.current != null && ref.version <= latestRef.current) return;
      latestRef.current = ref.version;
      // 保存した下書きを開いて試せるようにする（使う人の画面は公開するまで変わらない）。
      setVersion(ref.version);
      setReloadKey((k) => k + 1);
      toast({
        title: `AI が v${ref.version} を保存しました`,
        description: editingRef.current
          ? "編集中の内容は残しています。保存すると、先に最新の内容を読み込むよう案内します。"
          : "下書きを開きました。公開するまで、使う人の画面は変わりません。",
      });
    },
    [id],
  );

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
      if (isVersionConflict(e)) {
        // 比べた最新のバージョンの後に、ほかの人が保存した。最新の番号を読み直して、もう一度押してもらう（GAP-41）。
        toast({
          variant: "destructive",
          title: "ほかの人が先にアプリを保存しました",
          description: "最新の内容を読み込みました。もう一度「この内容を下書きにする」を押してください。",
        });
        setReloadKey((k) => k + 1);
        return;
      }
      toast({
        variant: "destructive",
        title: "下書きを保存できませんでした",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setRestoring(false);
    }
  };

  /// アプリを付けた会話を始める（束ねたスキルで答える・D-16 の (b)）。開いているバージョンを付ける。
  const askAi = async () => {
    if (!app) return;
    setAsking(true);
    try {
      const thread = await createThread(app.body.description, false, {
        miniApp: { artifactId: app.id, version: app.version },
      });
      router.push(`/c/${thread.id}`);
    } catch (e) {
      setAsking(false);
      const code = errorCode(e);
      if (code === "app_version_changed" || code === "not_published") {
        // 開いた後に公開したバージョンが変わった（使う人）。公開しているバージョンを開き直し、もう一度押してもらう。
        toast({
          title: "公開しているバージョンが変わりました",
          description: "画面を開き直しました。もう一度「このアプリについて AI に聞く」を押してください。",
        });
        setVersion(undefined);
        setReloadKey((k) => k + 1);
        return;
      }
      toast({
        variant: "destructive",
        title: "AI との会話を始められませんでした",
        // サーバーが理由を言うとき（束ねたスキルを使えないなど）はその文を、言わないときは状態コードを出さずに案内する。
        description:
          validationMessages(e) ?? (code && e instanceof Error ? e.message : "時間をおいて、もう一度試してください。"),
      });
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
  const seenRuns = React.useRef<Map<string, boolean> | null>(null);
  React.useEffect(() => {
    if (runsLoaded === 0) return;
    const { seen, finished } = runsFinishedSince(seenRuns.current, runs);
    seenRuns.current = seen;
    if (finished) setGridRevision((n) => n + 1);
  }, [runs, runsLoaded]);

  // 使えない部品（壊れた任意のピン）のうちスキルの分。画面とボタンはそのまま使える（BUG-17）。
  const skillUnavailable = app?.unavailable.find((u) => u.path === "skill");

  // 自分に足りない権限（GAP-37）。開いたバージョンごとに確かめ、押す前にボタンと表を止めて理由を出す。止めたボタンを
  // 押したとき・画面に戻ったときに確かめ直し、共有してもらえば開き直さずに使える（BUG-217・use-my-access）。
  // 読めなかったときは止めない（押したときにサーバーが本人の権限で確かめる）。
  const { access, check: checkAccess } = useMyAppAccess(app?.id ?? null, app?.version ?? null);
  const partAccess = React.useCallback(
    (kind: "workflow" | "data_view", alias: string) => findPart(access?.parts, kind, alias),
    [access],
  );
  const skillAccess = access?.parts.find((p) => p.part.kind === "skill") ?? null;

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
            // 自分に足りない権限があるボタンは押す前に止める（GAP-37）。押されたら確かめ直す（BUG-217）。
            blockedReason: (name) => {
              const p = partAccess("workflow", name);
              return p ? myBlockedReason(p) : null;
            },
            recheckBlocked: async (name) => {
              const fresh = await checkAccess(RECHECK_ON_PRESS_MS);
              // 確かめられなければ止めない（サーバーが本人の権限で確かめる）。
              const p = fresh ? findPart(fresh.parts, "workflow", name) : null;
              return p ? myBlockedReason(p) : null;
            },
            // 見つからないワークフローは、共有してもらっても押せるようにならない。
            fixableBySharing: (name) => {
              const p = partAccess("workflow", name);
              return p != null && holdsBack(p);
            },
          }
        : null,
    [app, runs, refreshRuns, partAccess, checkAccess],
  );

  // 確かめ直して表の見え方（見られない・自分が追加したものだけ・すべて）が変わったら、表を先頭から読み直す（BUG-217）。
  // 最初の確かめの結果では読み直さない（表が先頭のページを読み直さない・GAP-37）。
  const gridAccessSeen = React.useRef<{ version: number; sig: string } | null>(null);
  React.useEffect(() => {
    if (!access) return;
    const sig = access.parts
      .filter((p) => p.part.kind === "data_view")
      .map((p) => `${"alias" in p.part ? p.part.alias : ""}:${p.status}`)
      .sort()
      .join("|");
    const before = gridAccessSeen.current;
    gridAccessSeen.current = { version: access.version, sig };
    if (before && before.version === access.version && before.sig !== sig) setGridRevision((n) => n + 1);
  }, [access]);

  // 画面の表（```data_grid）は束ねた保存ビューを閲覧者本人の権限でページごとに実行する（Task 15.5）。
  // 起動が終わるたびに実行先を作り直し、表を読み直させる（BUG-19）。権限の確かめの結果は実行先と分けて足し、
  // 結果が届いても実行先（run）は変えない（表が先頭のページを読み直さない）。
  const gridRun = React.useMemo(
    () => (app ? miniAppDataGridHost(app.id, app.version, gridRevision) : null),
    [app, gridRevision],
  );
  const gridHost = React.useMemo(
    () =>
      gridRun
        ? {
            ...gridRun,
            blockedReason: (alias: string) => {
              const p = partAccess("data_view", alias);
              return p ? myBlockedReason(p) : null;
            },
            ownRecordsOnly: (alias: string) => partAccess("data_view", alias)?.status === "limited",
            // 共有してもらった後に確かめ直す（BUG-217）。見つからないビューは、共有しても直らない。
            recheck: async () => {
              await checkAccess(RECHECK_ON_PRESS_MS);
            },
            fixableBySharing: (alias: string) => {
              const p = partAccess("data_view", alias);
              return p != null && holdsBack(p);
            },
          }
        : null,
    [gridRun, partAccess, checkAccess],
  );

  // 本文（アプリの画面）。AI のパネルを開いている間も、本文の列だけをスクロールする。
  const body = (
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
            {app.body.skill ? (
              <div className="mt-3 flex flex-wrap items-center gap-x-3 gap-y-1">
                <Button
                  size="sm"
                  variant="outline"
                  disabled={asking || skillUnavailable != null}
                  onClick={() => void askAi()}
                  data-testid="miniapp-ask-ai"
                >
                  {asking ? <Loader2 className="size-4 animate-spin" aria-hidden /> : <MessageSquare className="size-4" aria-hidden />}
                  このアプリについて AI に聞く
                </Button>
                {skillUnavailable ? (
                  <span className="text-xs text-muted-foreground">スキルを使えないため、AI に聞けません。</span>
                ) : skillAccess ? (
                  <span className="text-xs text-muted-foreground" data-testid="miniapp-skill-limited">
                    参照するフォルダーやファイルのうち、あなたが見られないものは、AI の答えに使われません。
                  </span>
                ) : null}
              </div>
            ) : null}
            {access?.truncated ? (
              <p className="mt-2 text-xs text-muted-foreground" role="note" data-testid="miniapp-access-truncated">
                このアプリは使うテーブルやファイルが多いため、一部のボタンと表は、前もって権限を確かめていません。
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
              access={{ load: () => checkMiniAppAccess(app.id), subject: "app" }}
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
                owner={canShare}
                onReloadLatest={() => void reloadLatestIntoEditor()}
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

  return (
    <div className="flex h-full min-h-0 flex-col">
      <AssistantLayout
        open={chatOpen && canEdit}
        onClose={() => setChatOpen(false)}
        label="アプリのアシスタント"
        title="AI に依頼"
        // 閉じても書きかけの依頼と会話の購読を残す（AI が保存した知らせを受け続ける）。
        keepMounted
        panel={
          app && canEdit ? (
            <MiniAppChatPanel
              appId={app.id}
              name={app.body.description}
              initialThreadId={deepLinkThreadId}
              onBuilderSaved={onBuilderSaved}
            />
          ) : null
        }
      >
        <div className="min-h-0 min-w-0 flex-1 overflow-y-auto overscroll-y-contain">
          {body}
        </div>
      </AssistantLayout>
    </div>
  );
}
