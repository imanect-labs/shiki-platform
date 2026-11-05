"use client";

/// ミニアプリの作成・編集ダイアログ（Task 6.11・#604）。
///
/// 画面は Yui の ```ui ブロックを含む Markdown。ボタンの実行先（`-> workflow 名前`）は、ここで
/// 束ねたワークフローの名前からだけ選べる（保存時にサーバーが確かめる）。画面の表（```data_grid）は、ここで束ねた
/// ビューから選ぶ（GAP-11）。ワークフロー・ビュー・スキルは選んだ時点の版で固定し、新しいバージョンがあれば差を見て
/// 上げられる（`miniapp-parts.tsx`）。
///
/// 画面は「組み立てる」（ドラッグ＆ドロップ・screen-builder.tsx）・「書く」（記法）・「プレビュー」の
/// どれでも直せる。正本はどれも同じ Markdown で、組み立てるのチェッカーにエラーがある間は保存しない。

import * as React from "react";
import dynamic from "next/dynamic";
import { Loader2, Plus, TriangleAlert } from "lucide-react";

import type { WorkflowChoice, WorkflowInputHint } from "@imanect/yui/editor";

// 別名の長さの上限は、サーバーの検証と同じ定義から生成する（`crates/gui/src/miniapp.rs`）。
import { MAX_ALIAS_CHARS, type MiniAppBody } from "@/generated/gui-spec";
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
import { PartPins, pinKey, type PartKind, type PinCheck } from "@/components/artifacts/miniapp-parts";
import { useArtifactList } from "@/hooks/use-artifact-list";
import { useMediaQuery } from "@/hooks/use-media-query";
import {
  createMiniApp,
  getArtifact,
  getMiniAppPinUpdates,
  getMiniAppWorkflowInputs,
  getSkill,
  isVersionConflict,
  updateMiniApp,
  validationMessages,
  type MiniAppVersion,
} from "@/lib/artifact-api";
import { appendDataGrid, dataGridAliases } from "@/lib/miniapp/screen-blocks";
import { inputNotes, toYuiHints } from "@/lib/miniapp/workflow-inputs";
import { cn } from "@/lib/utils";

type Pin = MiniAppBody["workflows"][number];

/// ビューの名前から、束ねた別名と重ならない別名を作る（画面の表が 1 行目に書く名前）。
function viewAlias(name: string, taken: readonly Pin[]): string {
  const base = [...name.trim()].slice(0, MAX_ALIAS_CHARS).join("").trim() || "ビュー";
  let alias = base;
  for (let n = 2; taken.some((p) => p.alias === alias); n++) {
    alias = `${[...base].slice(0, MAX_ALIAS_CHARS - 4).join("").trim()} ${n}`;
  }
  return alias;
}

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
  const viewList = useArtifactList("data_view", { enabled: open });
  const skills = React.useMemo(() => skillList.items ?? [], [skillList.items]);
  const workflows = React.useMemo(() => workflowList.items ?? [], [workflowList.items]);
  const viewChoices = React.useMemo(() => viewList.items ?? [], [viewList.items]);
  const [pinnedSkill, setPinnedSkill] = React.useState<PinnedSkill>({ state: "checking" });
  const [skillId, setSkillId] = React.useState("");
  const [pins, setPins] = React.useState<Pin[]>([]);
  const [workflowToAdd, setWorkflowToAdd] = React.useState("");
  /// 束ねたビュー（画面の表 ```data_grid が出すもの・GAP-11）。
  const [views, setViews] = React.useState<Pin[]>([]);
  const [viewToAdd, setViewToAdd] = React.useState("");
  /// 部品の版ごとの、新しいバージョンの確かめの結果（`pinKey`）。
  const [checks, setChecks] = React.useState<Record<string, PinCheck>>({});
  /// 組み立てるに、表の段を足してもらう頼み（ビューを追加したとき・番号が変わるたびに 1 回）。
  const [gridRequest, setGridRequest] = React.useState<{ alias: string; n: number } | null>(null);
  /// 読みに行った部品の版（新しいバージョンの確かめ・入力の手がかり）。開き直すたびと、編集するアプリが替わるたび
  /// （ほかの人が先に保存して最新の内容を読み込んだとき）に空にする（結果を空にしたのに読み直さない、を作らない）。
  const checked = React.useRef(new Set<string>());
  const requested = React.useRef(new Set<string>());
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
    setViews(app?.body.data_views ?? []);
    setViewToAdd("");
    setChecks({});
    setGridRequest(null);
    checked.current.clear();
    requested.current.clear();
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

  /// ビューを束ねる。画面に、そのビューを出す表がまだ無ければ最後に置く（束ねたビューを画面で使える形にする）。
  const addView = () => {
    const v = viewChoices.find((x) => x.id === viewToAdd);
    if (!v) return;
    const alias = viewAlias(v.name, views);
    setViews((prev) => [...prev, { alias, artifact_id: v.id, version: v.currentVersion }]);
    setViewToAdd("");
    if (dataGridAliases(screen).includes(alias)) return;
    // 書くタブで直した記述は組み立てるの段より新しいので、記述に足す（組み立てるは次に開くときに分け直す）。
    if (sourceEdited.current) setScreen((s) => appendDataGrid(s, alias));
    else setGridRequest((r) => ({ alias, n: (r?.n ?? 0) + 1 }));
  };

  // 束ねた部品（ワークフロー・ビュー）の新しいバージョンと、上げると変わるところ。版ごとに 1 回だけ読む（読めなくても
  // 保存はできる。新しいバージョンが出ないだけ）。
  React.useEffect(() => {
    if (!open) return;
    const want = (kind: PartKind, list: readonly Pin[]) => list.filter((p) => !checked.current.has(pinKey(kind, p)));
    const wantWorkflows = want("workflow", pins);
    const wantViews = want("data_view", views);
    if (wantWorkflows.length === 0 && wantViews.length === 0) return;
    const keys = [
      ...wantWorkflows.map((p) => pinKey("workflow", p)),
      ...wantViews.map((p) => pinKey("data_view", p)),
    ];
    for (const key of keys) checked.current.add(key);
    const ref = (p: Pin) => ({ artifact_id: p.artifact_id, version: p.version });
    void getMiniAppPinUpdates({ workflows: wantWorkflows.map(ref), data_views: wantViews.map(ref) })
      .then((res) =>
        setChecks((m) => {
          const next = { ...m };
          [...res.workflows, ...res.data_views].forEach((u, i) => {
            next[keys[i]!] = u.readable ? { state: "readable", update: u } : { state: "unreadable" };
          });
          return next;
        }),
      )
      .catch(() =>
        setChecks((m) => ({ ...m, ...Object.fromEntries(keys.map((k) => [k, { state: "unknown" } as const])) })),
      );
  }, [pins, views, open]);
  const setVersion = (kind: PartKind) => (artifactId: string, version: number) =>
    (kind === "workflow" ? setPins : setViews)((prev) =>
      prev.map((p) => (p.artifact_id === artifactId ? { ...p, version } : p)),
    );

  // 束ねたワークフローの入力の手がかり（組み立てるの部品の候補と、画面の入力との突き合わせのため）。手がかりは
  // サーバーが入力項目から作る（保存のときの突き合わせと同じもの）。版ごとに 1 回だけ読む（読めなくても組み立ては
  // 使える。候補が出ないだけ）。
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
  // 画面の表が出しているのに、束ねていないビュー（保存すると、サーバーの検証で止まる）。
  const viewAliases = React.useMemo(() => views.map((v) => v.alias), [views]);
  const missingViews = React.useMemo(
    () => [...new Set(dataGridAliases(deferredScreen))].filter((a) => a && !viewAliases.includes(a)),
    [deferredScreen, viewAliases],
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
      // 画面の表（```data_grid）が出すビュー（読めないビューも、変えなければそのまま保存できる・BUG-80）。
      ...(views.length ? { data_views: views } : {}),
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
            画面は「組み立てる」でドラッグして作るか、「書く」で Markdown と ```ui の図で書きます。ボタンの実行先は下で追加したワークフローから、表は下で追加したビューから選べます。保存した内容は、公開するまで共有した人には見えません。
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
            <PartPins
              kind="workflow"
              pins={pins}
              saved={app?.body.workflows ?? []}
              checks={checks}
              onUpgrade={setVersion("workflow")}
              onRevert={setVersion("workflow")}
              onRemove={(id) => setPins((prev) => prev.filter((x) => x.artifact_id !== id))}
            />
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
                <option value="">ワークフローを選ぶ</option>
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
                ワークフローを追加
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
                views={viewAliases}
                appendGrid={gridRequest}
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

          <div className="space-y-1.5" data-testid="miniapp-views">
            <span className={FIELD}>表に出すビュー</span>
            <PartPins
              kind="data_view"
              pins={views}
              saved={app?.body.data_views ?? []}
              checks={checks}
              onUpgrade={setVersion("data_view")}
              onRevert={setVersion("data_view")}
              onRemove={(id) => setViews((prev) => prev.filter((x) => x.artifact_id !== id))}
            />
            {missingViews.map((alias) => (
              <p key={alias} role="note" className="text-xs text-amber-700 dark:text-amber-400" data-testid="miniapp-view-missing">
                画面の表「{alias}」のビューを追加していません。このままでは保存できないので、ビューを追加するか、画面から表を消してください。
              </p>
            ))}
            <div className="flex items-center gap-2">
              <select
                aria-label="追加するビュー"
                value={viewToAdd}
                onChange={(e) => setViewToAdd(e.target.value)}
                className={cn(SELECT, "min-w-0 flex-1")}
              >
                <option value="">ビューを選ぶ</option>
                {viewChoices
                  .filter((v) => !views.some((p) => p.artifact_id === v.id))
                  .map((v) => (
                    <option key={v.id} value={v.id}>
                      {v.name}（v{v.currentVersion}）
                    </option>
                  ))}
              </select>
              <Button type="button" size="sm" variant="outline" disabled={!viewToAdd} onClick={addView}>
                <Plus className="size-4" aria-hidden />
                ビューを追加
              </Button>
            </div>
            {viewList.error ? (
              <ListLoadFailed what="ビューの一覧" compact onRetry={viewList.reload} />
            ) : (
              <ListMore noun="ビュー" found={viewChoices.length > 0} list={viewList} compact />
            )}
            <p className="text-xs text-muted-foreground">
              ビュー（テーブルの見え方）は、テーブルの絞り込みや並べ替えを保存したものです。追加すると、画面の最後に表を置きます。表のレコードは、アプリを使う人の権限で読み込みます。
            </p>
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
