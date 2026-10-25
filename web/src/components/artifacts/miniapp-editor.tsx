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
import { Loader2, Plus, X } from "lucide-react";

import type { WorkflowChoice } from "@imanect/yui/editor";

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
import { useMediaQuery } from "@/hooks/use-media-query";
import {
  createMiniApp,
  listArtifacts,
  updateMiniApp,
  type ArtifactMeta,
  type MiniAppVersion,
} from "@/lib/artifact-api";
import { inputHints } from "@/lib/miniapp/workflow-inputs";
import { cn } from "@/lib/utils";
import { getWorkflowVersion } from "@/lib/workflow-api";

type Pin = MiniAppBody["workflows"][number];

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
  app,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSaved: (saved: MiniAppVersion) => void;
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
  const [inputs, setInputs] = React.useState<Record<string, WorkflowChoice["inputs"]>>({});
  const [skills, setSkills] = React.useState<ArtifactMeta[]>([]);
  const [workflows, setWorkflows] = React.useState<ArtifactMeta[]>([]);
  const [skillId, setSkillId] = React.useState("");
  const [pins, setPins] = React.useState<Pin[]>([]);
  const [workflowToAdd, setWorkflowToAdd] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);

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
    void listArtifacts("skill").then(setSkills).catch(() => setSkills([]));
    void listArtifacts("workflow").then(setWorkflows).catch(() => setWorkflows([]));
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

  // 束ねたワークフローの入力の定義（組み立てるで部品の候補と、足りない必須の入力を出すため）。
  // 版ごとに 1 回だけ読む（読めなくても組み立ては使える。候補が出ないだけ）。
  const requested = React.useRef(new Set<string>());
  React.useEffect(() => {
    if (!open) requested.current.clear();
  }, [open]);
  React.useEffect(() => {
    if (!open) return;
    for (const p of pins) {
      const key = `${p.artifact_id}@${p.version}`;
      if (requested.current.has(key)) continue;
      requested.current.add(key);
      void getWorkflowVersion(p.artifact_id, p.version)
        .then((w) => setInputs((m) => ({ ...m, [key]: inputHints(w.ir.input_schema) })))
        .catch(() => setInputs((m) => ({ ...m, [key]: [] })));
    }
  }, [pins, open]);
  const choices = React.useMemo<WorkflowChoice[]>(() => pins.map((p) => ({ name: p.alias, inputs: inputs[`${p.artifact_id}@${p.version}`] })), [pins, inputs]);
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
    try {
      const saved = editing
        ? await updateMiniApp(app.id, body, app.version)
        : await createMiniApp(name.trim(), body);
      onSaved(saved);
      onOpenChange(false);
    } catch (err) {
      setError(err instanceof Error ? err.message : "保存に失敗しました");
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
            画面は「組み立てる」でドラッグして作るか、「書く」で Markdown と ```ui の図で書きます。ボタンの実行先は、下で束ねたワークフローから選べます。
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
                className="block w-full resize-y rounded-lg border border-input bg-background px-3 py-2 font-mono text-[13px] leading-relaxed focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              />
            )}
          </div>

          <div className="space-y-1.5">
            <label htmlFor="app-skill" className={FIELD}>
              スキル（任意）
            </label>
            <select id="app-skill" value={skillId} onChange={(e) => setSkillId(e.target.value)} className={SELECT}>
              <option value="">なし</option>
              {skills.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}（v{s.currentVersion}）
                </option>
              ))}
            </select>
          </div>

          {error ? (
            <p
              className="whitespace-pre-line rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive"
              role="alert"
            >
              {error}
            </p>
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
