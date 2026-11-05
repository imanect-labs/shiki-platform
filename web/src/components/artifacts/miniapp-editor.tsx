"use client";

/// ミニアプリの作成・編集ダイアログ（Task 6.11・#604）。
///
/// 画面は Yui の ```ui ブロックを含む Markdown。ボタンの実行先（`-> workflow 名前`）は、ここで
/// 束ねたワークフローの名前からだけ選べる（保存時にサーバーが確かめる）。ワークフローとスキルは
/// 選んだ時点の版で固定する。
///
/// 画面は「組み立てる」（ドラッグ＆ドロップ・screen-builder.tsx）・「書く」（記法）・「プレビュー」の
/// どれでも直せる。正本はどれも同じ Markdown で、組み立てるのチェッカーにエラーがある間は保存しない。

import * as React from "react";
import dynamic from "next/dynamic";
import { Loader2, Plus, TriangleAlert, X } from "lucide-react";

import type { WorkflowChoice, WorkflowInputHint } from "@imanect/yui/editor";

import type { MiniAppBody } from "@/generated/gui-spec";
import type { ScreenValidity } from "@/components/artifacts/screen-builder";
import { Markdown } from "@/components/prompt-kit/markdown";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { ListLoadFailed, ListMore } from "@/components/artifacts/shared-more";
import { useArtifactList } from "@/hooks/use-artifact-list";
import { useMediaQuery } from "@/hooks/use-media-query";
import {
  createMiniApp,
  getArtifact,
  getMiniAppWorkflowInputs,
  getSkill,
  isVersionConflict,
  updateMiniApp,
  validationMessages,
  type MiniAppVersion,
} from "@/lib/artifact-api";
import { inputNotes, toYuiHints } from "@/lib/miniapp/workflow-inputs";
import { cn } from "@/lib/utils";

type Pin = MiniAppBody["workflows"][number];

/// 束ねているスキルのピンを読めるか（スキルの一覧は読み込んだページの分しか無いので、一覧に無いことでは決めない・
/// ピンの版を直に読んで、404・403 のときだけ読めないとする・BUG-17）。
type PinnedSkill =
  | { state: "checking" }
  | { state: "readable"; name: string | null }
  | { state: "unreadable" }
  /// 通信・サーバの誤りで決められない（読めないと決めつけない）。
  | { state: "unknown" };
// 読めない部品も、変えなければそのまま保存できる（保存のときに確かめるのは足した部品と、対象か版を変えた部品だけ・
// D-16 の (d)・BUG-80）。

/// 束ねたワークフローの入力の定義を読めたか（読めないときは「入力はありません」と扱わない・BUG-122）。`hints` が
/// `undefined` は、入力項目を決めていないワークフロー（突き合わせない）。
type PinnedInputs =
  | { state: "readable"; hints: WorkflowInputHint[] | undefined }
  /// 403・404（削除されたか、見る権限がない。共同編集者がワークフローを見られないときなど）。
  | { state: "unreadable" }
  /// 通信・サーバの誤りで決められない。
  | { state: "unknown" };

/// 誤りの状態コード（`artifact-api`・`workflow-api` の `ok` が添える）。
function statusOf(e: unknown): number | null {
  const status = (e as { status?: unknown } | null)?.status;
  return typeof status === "number" ? status : null;
}

/// 新しいアプリの画面の雛形（最初に選んだワークフローの名前を入れる）。
function templateScreen(workflow: string): string {
  // 画面の見出しはページが出すので、本文は説明から始める。
  return [
    "内容を入れて「申請する」を押してください。",
    "",
    "```ui",
    "app 経費の申請",
    "金額 = number 必須 円 as amount",
    "区分 = choice 交通費 | 交際費 as category",
    `button 申請する -> workflow ${workflow}`,
    "```",
    "",
  ].join("\n");
}

const FIELD = "block text-xs font-medium text-foreground/70";
// 組み立てる（Yui のエディタ）はダイアログを開いたときだけ読む（一覧・実行画面の最初の読み込みを重くしない）
const ScreenBuilder = dynamic(() => import("@/components/artifacts/screen-builder").then((m) => m.ScreenBuilder), {
  ssr: false,
  loading: () => (
    <div className="flex min-h-64 items-center justify-center rounded-lg border border-border/60 text-sm text-muted-foreground">
      <Loader2 className="mr-2 size-4 animate-spin" aria-hidden />
      読み込み中…
    </div>
  ),
});

type Tab = "build" | "write" | "preview";
const TABS: { id: Tab; label: string }[] = [
  { id: "build", label: "組み立てる" },
  { id: "write", label: "書く" },
  { id: "preview", label: "プレビュー" },
];

const SELECT =
  "h-9 w-full rounded-lg border border-input bg-background px-3 text-base md:text-sm pointer-coarse:h-11 pointer-coarse:text-base";

export function MiniAppEditorDialog({
  open,
  onOpenChange,
  onSaved,
  onReloadLatest,
  app,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSaved: (saved: MiniAppVersion) => void;
  /// ほかの人が先に保存したとき、最新の内容をエディターに読み込み直す（保存していない変更は消える）。
  onReloadLatest?: () => void;
  /// 編集するアプリ（無ければ新規作成）。
  app?: { id: string; version: number; body: MiniAppBody };
}) {
  const editing = app != null;
  // タッチで開いた瞬間にキーボードを出さない（マウス操作のときだけ自動フォーカス）。
  const finePointer = useMediaQuery("(pointer: fine)");
  const [name, setName] = React.useState("");
  const [description, setDescription] = React.useState("");
  const [screen, setScreen] = React.useState("");
  const [tab, setTab] = React.useState<Tab>("build");
  // 画面が外から変わったとき（雛形を入れた）に組み立てるを作り直すための印
  const [builderKey, setBuilderKey] = React.useState(0);
  const [validity, setValidity] = React.useState<ScreenValidity>({ errors: 0 });
  // 書くタブで記述を直したか（組み立てるの段は、次に開くときに分け直す）
  const sourceEdited = React.useRef(false);
  const [inputs, setInputs] = React.useState<Record<string, PinnedInputs>>({});
  // 選択肢は開くたびに読み直す。50 件より古い自分のものと、共有された分の続きは「さらに読み込む」で足す。読み込みの
  // 失敗は「なし」だけの選択肢に見せない（BUG-186・BUG-189）。
  const skillList = useArtifactList("skill", { enabled: open });
  const workflowList = useArtifactList("workflow", { enabled: open });
  const skills = React.useMemo(() => skillList.items ?? [], [skillList.items]);
  const workflows = React.useMemo(() => workflowList.items ?? [], [workflowList.items]);
  const [pinnedSkill, setPinnedSkill] = React.useState<PinnedSkill>({ state: "checking" });
  const [skillId, setSkillId] = React.useState("");
  const [pins, setPins] = React.useState<Pin[]>([]);
  const [workflowToAdd, setWorkflowToAdd] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  /// 比べたバージョンの後に、ほかの人が保存した（付録 C の §3.5・GAP-41）。
  const [conflict, setConflict] = React.useState(false);

  React.useEffect(() => {
    if (!open) return;
    setName("");
    setDescription(app?.body.description ?? "");
    setScreen(app?.body.screen ?? "");
    setSkillId(app?.body.skill?.artifact_id ?? "");
    setPins(app?.body.workflows ?? []);
    setWorkflowToAdd("");
    setTab("build");
    setValidity({ errors: 0 });
    sourceEdited.current = false;
    // 開くたびに組み立てるを作り直す（前に開いたときの段を、別のアプリや取り消した編集のまま残さない）
    setBuilderKey((k) => k + 1);
    setInputs({});
    setError(null);
    setConflict(false);
    // 束ねているスキルは一覧（読み込んだページの分だけ）に有るかでなく、ピンの版を直に読んで確かめる。
    setPinnedSkill({ state: "checking" });
    const pin = app?.body.skill;
    if (!pin) return;
    let live = true;
    void getSkill(pin.artifact_id, pin.version)
      .then(async () => {
        // 名前は選択欄に出すためだけに読む（読めなくても、版が読めたので読めるスキルとして扱う）。
        const name = await getArtifact(pin.artifact_id).then(
          (m) => m.name,
          () => null,
        );
        if (live) setPinnedSkill({ state: "readable", name });
      })
      .catch((e: unknown) => {
        const status = statusOf(e);
        if (live) setPinnedSkill({ state: status === 403 || status === 404 ? "unreadable" : "unknown" });
      });
    return () => {
      live = false;
    };
  }, [open, app]);

  const addWorkflow = () => {
    const w = workflows.find((x) => x.id === workflowToAdd);
    if (!w) return;
    setPins((prev) => [...prev, { alias: w.name, artifact_id: w.id, version: w.currentVersion }]);
    // 画面が空なら、選んだワークフローを起動する雛形を入れる（書き方の手がかり）。
    setScreen((s) => {
      if (s.trim()) return s;
      setBuilderKey((k) => k + 1);
      return templateScreen(w.name);
    });
    setWorkflowToAdd("");
  };

  // 束ねたワークフローの入力の手がかり（組み立てるの部品の候補と、画面の入力との突き合わせのため）。手がかりは
  // サーバーが入力項目から作る（保存のときの突き合わせと同じもの）。版ごとに 1 回だけ読む（読めなくても組み立ては
  // 使える。候補が出ないだけ）。
  const requested = React.useRef(new Set<string>());
  React.useEffect(() => {
    if (!open) requested.current.clear();
  }, [open]);
  React.useEffect(() => {
    if (!open) return;
    const wanted = pins.filter((p) => !requested.current.has(`${p.artifact_id}@${p.version}`));
    if (wanted.length === 0) return;
    const keys = wanted.map((p) => `${p.artifact_id}@${p.version}`);
    for (const key of keys) requested.current.add(key);
    void getMiniAppWorkflowInputs(wanted.map((p) => ({ artifact_id: p.artifact_id, version: p.version })))
      .then((items) =>
        setInputs((m) => {
          const next = { ...m };
          items.forEach((item, i) => {
            next[keys[i]!] = item.readable
              ? { state: "readable", hints: item.inputs ? toYuiHints(item.inputs) : undefined }
              : { state: "unreadable" };
          });
          return next;
        }),
      )
      .catch(() =>
        // 通信・サーバの誤りで決められない（読めないと決めつけない）。
        setInputs((m) => ({ ...m, ...Object.fromEntries(keys.map((k) => [k, { state: "unknown" } as const])) })),
      );
  }, [pins, open]);
  // 読めなかったワークフローは入力を渡さない（空の入力を渡すと、組み立てるが画面のどの入力にも「という入力は
  // ありません」と言う。入力が無いのではなく、読めないだけ・BUG-122）。
  const choices = React.useMemo<WorkflowChoice[]>(
    () =>
      pins.map((p) => {
        const s = inputs[`${p.artifact_id}@${p.version}`];
        return { name: p.alias, inputs: s?.state === "readable" ? s.hints : undefined };
      }),
    [pins, inputs],
  );
  const unreadablePins = pins.flatMap((p) => {
    const s = inputs[`${p.artifact_id}@${p.version}`];
    return s && s.state !== "readable" ? [{ alias: p.alias, state: s.state }] : [];
  });
  // 書くタブの記述の、画面の入力とワークフローの入力項目の食い違い（組み立てると同じ突き合わせ・保存は止めない）。
  const deferredScreen = React.useDeferredValue(screen);
  const writeNotes = React.useMemo(
    () => (tab === "write" ? inputNotes(deferredScreen, choices) : []),
    [tab, deferredScreen, choices],
  );
  // 書くタブで直した後は、隠れている組み立てる（直す前の段）の数え直しを使わない（ワークフローを外したときなど）
  const onValidity = React.useCallback((v: ScreenValidity) => {
    if (!sourceEdited.current) setValidity(v);
  }, []);
  // 組み立てるで出たエラーはタブを移っても保存を止める（プレビューへ移るだけで保存できないように）。
  // 外れるのは組み立てるで直したときか、書くタブで記述を直したとき（そのときは書いた記述を保存時のサーバーが確かめる）
  const blocked = validity.errors > 0;
  // 書くタブで直した記述は、次に組み立てるを開くときに段へ分け直す
  const selectTab = (next: Tab) => {
    if (next === "build" && sourceEdited.current) {
      sourceEdited.current = false;
      setBuilderKey((k) => k + 1);
    }
    setTab(next);
  };

  // 束ねているスキルが一覧に無いとき（まだ読み込んでいないページ・削除された・自分には読めない）も選択欄に残して見せる
  // （一覧に無いと選択欄は「なし」に見えてしまう）。読めないと確かめられたときだけ、選び直しを促す（BUG-17）。
  const pin = app?.body.skill ?? null;
  const pinOutsideList = pin != null && !skills.some((s) => s.id === pin.artifact_id) ? pin : null;
  const missingSkill =
    pinOutsideList != null && skillId === pinOutsideList.artifact_id && pinnedSkill.state === "unreadable"
      ? pinOutsideList
      : null;
  const pinLabel = (p: NonNullable<MiniAppBody["skill"]>) =>
    pinnedSkill.state === "unreadable"
      ? `読めないスキル（v${p.version}）`
      : pinnedSkill.state === "readable" && pinnedSkill.name
        ? `${pinnedSkill.name}（v${p.version}）`
        : `束ねているスキル（v${p.version}）`;

  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    if (busy || blocked) return;
    const current = app?.body.skill;
    const skill = skills.find((s) => s.id === skillId);
    const body: MiniAppBody = {
      description: description.trim(),
      screen,
      // 編集で同じスキルのままなら版も据え置く（勝手に最新へ上げない）。
      skill:
        skillId === ""
          ? null
          : current && current.artifact_id === skillId
            ? current
            : skill
              ? { artifact_id: skill.id, version: skill.currentVersion }
              : null,
      workflows: pins,
      // 画面の表（```data_grid）が参照する保存ビューのピンは、この画面では編集しないので
      // 版も含めてそのまま引き継ぐ（保存で落とすと表が描けなくなる・Task 15.7）。
      ...(app?.body.data_views?.length ? { data_views: app.body.data_views } : {}),
    };
    setBusy(true);
    setError(null);
    setConflict(false);
    try {
      const saved = editing
        ? await updateMiniApp(app.id, body, app.version)
        : await createMiniApp(name.trim(), body);
      onSaved(saved);
      onOpenChange(false);
    } catch (err) {
      // ほかの人が先に保存した: 変更は残したまま、最新の内容を読み込むかを選んでもらう（§3.5）。
      if (isVersionConflict(err)) {
        setConflict(true);
        return;
      }
      // 検証エラーは 1 件 1 行で、位置（workflows[0] など）を添えずに出す（BUG-80）。名前の重複などの 409 は
      // サーバーの文言（何が起きたかと直し方）を出す（GAP-41）。
      setError(validationMessages(err) ?? (err instanceof Error ? err.message : "保存できませんでした。"));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className={cn("max-h-[92dvh] overflow-y-auto", tab === "build" ? "max-w-6xl" : "max-w-3xl")}>
        <DialogHeader>
          <DialogTitle>{editing ? "アプリを編集" : "アプリを作成"}</DialogTitle>
          <DialogDescription>
            画面は「組み立てる」でドラッグして作るか、「書く」で Markdown と ```ui の図で書きます。ボタンの実行先は、下で束ねたワークフローから選べます。保存した内容は、公開するまで共有した人には見えません。
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={save} className="space-y-4">
          {editing ? null : (
            <div className="space-y-1.5">
              <label htmlFor="app-name" className={FIELD}>
                名前<span className="ml-0.5 text-destructive">*</span>
              </label>
              <Input
                id="app-name"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="expense-app"
                required
                autoFocus={finePointer}
                autoCapitalize="none"
                autoCorrect="off"
                spellCheck={false}
              />
            </div>
          )}
          <div className="space-y-1.5">
            <label htmlFor="app-desc" className={FIELD}>
              画面の見出し<span className="ml-0.5 text-destructive">*</span>
            </label>
            <Input
              id="app-desc"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="経費の申請"
              required
            />
          </div>

          <div className="space-y-1.5">
            <span className={FIELD}>ワークフロー</span>
            <div className="flex flex-wrap items-center gap-1.5">
              {pins.map((p) => (
                <span
                  key={p.artifact_id}
                  className="inline-flex items-center gap-1 rounded-full border border-border bg-secondary/60 px-2.5 py-1 text-xs"
                >
                  <code className="font-mono">{p.alias}</code>（v{p.version}）
                  <button
                    type="button"
                    aria-label={`${p.alias} を外す`}
                    onClick={() => setPins((prev) => prev.filter((x) => x.artifact_id !== p.artifact_id))}
                    className="relative -mr-1 flex size-5 items-center justify-center rounded-full text-muted-foreground hover:text-destructive pointer-coarse:after:absolute pointer-coarse:after:-inset-3 pointer-coarse:after:content-['']"
                  >
                    <X className="size-3" aria-hidden />
                  </button>
                </span>
              ))}
            </div>
            {unreadablePins.map((u) => (
              <p
                key={u.alias}
                role="note"
                data-testid="miniapp-workflow-unreadable"
                className="text-xs text-amber-700 dark:text-amber-400"
              >
                {u.state === "unreadable"
                  ? `ワークフロー「${u.alias}」が見つかりません。削除されたか、見る権限がありません。入力項目と画面の入力が合うかは、確かめていません。`
                  : `ワークフロー「${u.alias}」の入力項目を読み込めませんでした。少し待ってから、開き直してください。`}
              </p>
            ))}
            <div className="flex items-center gap-2">
              <select
                aria-label="追加するワークフロー"
                value={workflowToAdd}
                onChange={(e) => setWorkflowToAdd(e.target.value)}
                className={cn(SELECT, "min-w-0 flex-1")}
              >
                <option value="">ワークフローを選択</option>
                {workflows
                  .filter((w) => !pins.some((p) => p.artifact_id === w.id))
                  .map((w) => (
                    <option key={w.id} value={w.id}>
                      {w.name}（v{w.currentVersion}）
                    </option>
                  ))}
              </select>
              <Button type="button" size="sm" variant="outline" disabled={!workflowToAdd} onClick={addWorkflow}>
                <Plus className="size-4" aria-hidden />
                追加
              </Button>
            </div>
            {workflowList.error ? (
              <ListLoadFailed what="ワークフローの一覧" compact onRetry={workflowList.reload} />
            ) : (
              <ListMore noun="ワークフロー" found={workflows.length > 0} list={workflowList} compact />
            )}
            <p className="text-xs text-muted-foreground">
              ボタンでは <code className="font-mono">button 申請する -&gt; workflow 名前</code> のように名前で指定します。
            </p>
          </div>

          <div className="space-y-1.5">
            <div className="flex items-center justify-between">
              <label htmlFor="app-screen" className={FIELD}>
                画面<span className="ml-0.5 text-destructive">*</span>
              </label>
              <div role="tablist" aria-label="画面の表示" className="flex gap-1 text-xs">
                {TABS.map((x) => (
                  <button
                    key={x.id}
                    type="button"
                    role="tab"
                    aria-selected={tab === x.id}
                    onClick={() => selectTab(x.id)}
                    className={cn(
                      "rounded-md px-2 py-1 transition-colors pointer-coarse:px-3 pointer-coarse:py-2",
                      tab === x.id ? "bg-secondary text-foreground" : "text-muted-foreground hover:text-foreground",
                    )}
                  >
                    {x.label}
                  </button>
                ))}
              </div>
            </div>
            {/* 組み立てるはタブを移っても消さずに隠す（段・選択・エラーを持ち続けるため） */}
            <div hidden={tab !== "build"}>
              <ScreenBuilder
                key={builderKey}
                screen={screen}
                onScreenChange={setScreen}
                workflows={choices}
                onValidity={onValidity}
                onOpenSource={() => selectTab("write")}
              />
            </div>
            {tab === "build" ? null : tab === "preview" ? (
              <div className="min-h-64 rounded-lg border border-border px-6 py-4" data-testid="miniapp-preview">
                {screen.trim() ? (
                  <Markdown>{screen}</Markdown>
                ) : (
                  <p className="text-sm text-muted-foreground">画面がまだ空です。</p>
                )}
              </div>
            ) : (
              <textarea
                id="app-screen"
                value={screen}
                onChange={(e) => {
                  setScreen(e.target.value);
                  sourceEdited.current = true;
                  setValidity({ errors: 0 });
                }}
                required
                rows={14}
                spellCheck={false}
                placeholder={"# 画面の見出し\n\n```ui\napp …\n```"}
                aria-describedby={writeNotes.length > 0 ? "app-screen-inputs" : undefined}
                className="block w-full resize-y rounded-lg border border-input bg-background px-3 py-2 font-mono text-[13px] leading-relaxed focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              />
            )}
            {tab === "write" && writeNotes.length > 0 ? (
              <div
                id="app-screen-inputs"
                role="note"
                data-testid="miniapp-write-input-notes"
                className="space-y-1 rounded-lg border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-xs text-amber-800 dark:text-amber-300"
              >
                <p className="flex items-start gap-1.5 font-medium">
                  <TriangleAlert className="mt-0.5 size-3.5 shrink-0" aria-hidden />
                  画面の入力が、ワークフローの入力項目と合っていません。保存はできます。
                </p>
                <ul className="list-disc space-y-0.5 pl-5">
                  {writeNotes.map((n, i) => (
                    <li key={i} className="[overflow-wrap:anywhere]">
                      {n}
                    </li>
                  ))}
                </ul>
              </div>
            ) : null}
          </div>

          <div className="space-y-1.5">
            <label htmlFor="app-skill" className={FIELD}>
              スキル（任意）
            </label>
            <select
              id="app-skill"
              value={skillId}
              onChange={(e) => setSkillId(e.target.value)}
              className={SELECT}
              aria-describedby={missingSkill ? "app-skill-missing" : undefined}
            >
              <option value="">なし</option>
              {pinOutsideList ? <option value={pinOutsideList.artifact_id}>{pinLabel(pinOutsideList)}</option> : null}
              {skills.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}（v{s.currentVersion}）
                </option>
              ))}
            </select>
            {skillList.error ? (
              <ListLoadFailed what="スキルの一覧" compact onRetry={skillList.reload} />
            ) : (
              <ListMore noun="スキル" found={skills.length > 0} list={skillList} compact />
            )}
            {missingSkill ? (
              <p id="app-skill-missing" className="text-xs text-amber-700 dark:text-amber-400" role="note">
                束ねているスキルは、削除されたか、あなたに見る権限がありません。変えなければ、このまま保存できます。
              </p>
            ) : null}
          </div>

          {error ? (
            <p
              className="whitespace-pre-line rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive"
              role="alert"
            >
              {error}
            </p>
          ) : null}
          {conflict ? (
            <div
              role="alert"
              data-testid="miniapp-save-conflict"
              className="space-y-2 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm"
            >
              <p className="font-medium text-destructive">ほかの人が先にアプリを保存しました</p>
              <p className="text-foreground/80">
                あなたの変更は保存されていません。最新の内容を読み込むと、保存していない変更は消えます。残したい内容は先に控えてください。
              </p>
              <div className="flex flex-wrap gap-2">
                {onReloadLatest ? (
                  <Button type="button" size="sm" variant="outline" onClick={onReloadLatest}>
                    最新の内容を読み込む（変更は消えます）
                  </Button>
                ) : null}
                <Button type="button" size="sm" variant="ghost" onClick={() => setConflict(false)}>
                  閉じる
                </Button>
              </div>
            </div>
          ) : null}

          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)} disabled={busy}>
              キャンセル
            </Button>
            {blocked ? (
              <span className="mr-auto self-center text-xs text-destructive" role="status">
                組み立てるに直す必要のある箇所が {validity.errors} 件あります
              </span>
            ) : null}
            <Button type="submit" disabled={busy || blocked}>
              {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
              {editing ? "保存" : "作成"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
