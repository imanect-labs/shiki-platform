"use client";

/// ミニアプリの作成・編集ダイアログ（Task 6.11・#604）。
///
/// 画面は Yui の ```ui ブロックを含む Markdown。ボタンの実行先（`-> workflow 名前`）は、ここで
/// 束ねたワークフローの名前からだけ選べる（保存時にサーバーが確かめる）。ワークフローとスキルは
/// 選んだ時点の版で固定する。

import * as React from "react";
import { Loader2, Plus, X } from "lucide-react";

import type { MiniAppBody } from "@/generated/gui-spec";
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
import { cn } from "@/lib/utils";

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
  const [preview, setPreview] = React.useState(false);
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
    setPreview(false);
    setError(null);
    void listArtifacts("skill").then(setSkills).catch(() => setSkills([]));
    void listArtifacts("workflow").then(setWorkflows).catch(() => setWorkflows([]));
  }, [open, app]);

  const addWorkflow = () => {
    const w = workflows.find((x) => x.id === workflowToAdd);
    if (!w) return;
    setPins((prev) => [...prev, { alias: w.name, artifact_id: w.id, version: w.currentVersion }]);
    // 画面が空なら、選んだワークフローを起動する雛形を入れる（書き方の手がかり）。
    setScreen((s) => (s.trim() ? s : templateScreen(w.name)));
    setWorkflowToAdd("");
  };

  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    if (busy) return;
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
      <DialogContent className="max-h-[92dvh] max-w-3xl overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{editing ? "アプリを編集" : "アプリを作成"}</DialogTitle>
          <DialogDescription>
            画面は Markdown と ```ui の図で書きます。ボタンの実行先は、下で束ねたワークフローの名前から選べます。
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
                {(["書く", "プレビュー"] as const).map((label, i) => (
                  <button
                    key={label}
                    type="button"
                    role="tab"
                    aria-selected={preview === (i === 1)}
                    onClick={() => setPreview(i === 1)}
                    className={cn(
                      "rounded-md px-2 py-1 transition-colors",
                      preview === (i === 1) ? "bg-secondary text-foreground" : "text-muted-foreground hover:text-foreground",
                    )}
                  >
                    {label}
                  </button>
                ))}
              </div>
            </div>
            {preview ? (
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
                onChange={(e) => setScreen(e.target.value)}
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
            <Button type="submit" disabled={busy}>
              {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
              {editing ? "保存" : "作成"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
