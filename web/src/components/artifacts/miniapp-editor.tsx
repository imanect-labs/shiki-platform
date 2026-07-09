"use client";

/// ミニアプリの作成ダイアログ（Task 6.11）。
/// 自分の UI スペック・スキル・ワークフローから選んで束ねる（版は現行版を明示ピン）。

import * as React from "react";
import { Loader2, Plus, X } from "lucide-react";

import type { MiniAppBody } from "@/generated/gui-spec";
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
import { createMiniApp, listArtifacts, type ArtifactMeta } from "@/lib/artifact-api";

export function MiniAppEditorDialog({
  open,
  onOpenChange,
  onSaved,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSaved: () => void;
}) {
  const [name, setName] = React.useState("");
  const [description, setDescription] = React.useState("");
  const [uiSpecs, setUiSpecs] = React.useState<ArtifactMeta[]>([]);
  const [skills, setSkills] = React.useState<ArtifactMeta[]>([]);
  const [workflows, setWorkflows] = React.useState<ArtifactMeta[]>([]);
  const [uiSpecId, setUiSpecId] = React.useState("");
  const [skillId, setSkillId] = React.useState("");
  const [selectedWorkflows, setSelectedWorkflows] = React.useState<ArtifactMeta[]>([]);
  const [workflowToAdd, setWorkflowToAdd] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);

  React.useEffect(() => {
    if (!open) return;
    setName("");
    setDescription("");
    setUiSpecId("");
    setSkillId("");
    setSelectedWorkflows([]);
    setWorkflowToAdd("");
    setError(null);
    void listArtifacts("ui_spec").then(setUiSpecs).catch(() => setUiSpecs([]));
    void listArtifacts("skill").then(setSkills).catch(() => setSkills([]));
    void listArtifacts("workflow").then(setWorkflows).catch(() => setWorkflows([]));
  }, [open]);

  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    if (busy) return;
    const uiSpec = uiSpecs.find((s) => s.id === uiSpecId);
    if (!uiSpec) {
      setError("UI スペックを選択してください");
      return;
    }
    const skill = skills.find((s) => s.id === skillId);
    const body: MiniAppBody = {
      description: description.trim(),
      ui_spec: { artifact_id: uiSpec.id, version: uiSpec.currentVersion },
      skill: skill ? { artifact_id: skill.id, version: skill.currentVersion } : null,
      workflows: selectedWorkflows.map((w) => ({
        alias: w.name,
        artifact_id: w.id,
        version: w.currentVersion,
      })),
    };
    setBusy(true);
    setError(null);
    try {
      await createMiniApp(name.trim(), body);
      onSaved();
      onOpenChange(false);
    } catch (err) {
      setError(err instanceof Error ? err.message : "作成に失敗しました");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>アプリを作成</DialogTitle>
          <DialogDescription>
            UI スペック・スキル・ワークフローを版固定で束ねます（部品は現行版がピンされます）。
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={save} className="space-y-4">
          <div className="space-y-1.5">
            <label htmlFor="app-name" className="block text-xs font-medium text-foreground/70">
              名前<span className="ml-0.5 text-destructive">*</span>
            </label>
            <Input
              id="app-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="expense-app"
              required
              autoFocus
            />
          </div>
          <div className="space-y-1.5">
            <label htmlFor="app-desc" className="block text-xs font-medium text-foreground/70">
              説明<span className="ml-0.5 text-destructive">*</span>
            </label>
            <Input
              id="app-desc"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="経費精算の申請と確認"
              required
            />
          </div>

          <div className="space-y-1.5">
            <label htmlFor="app-uispec" className="block text-xs font-medium text-foreground/70">
              UI スペック<span className="ml-0.5 text-destructive">*</span>
            </label>
            <select
              id="app-uispec"
              value={uiSpecId}
              onChange={(e) => setUiSpecId(e.target.value)}
              required
              className="h-9 w-full rounded-lg border border-input bg-background px-3 text-sm"
            >
              <option value="">選択してください</option>
              {uiSpecs.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}（v{s.currentVersion}）
                </option>
              ))}
            </select>
            {uiSpecs.length === 0 ? (
              <p className="text-xs text-muted-foreground">
                UI スペックがまだありません。チャットで生成した UI を保存するか API で作成してください。
              </p>
            ) : null}
          </div>

          <div className="space-y-1.5">
            <label htmlFor="app-skill" className="block text-xs font-medium text-foreground/70">
              スキル（任意）
            </label>
            <select
              id="app-skill"
              value={skillId}
              onChange={(e) => setSkillId(e.target.value)}
              className="h-9 w-full rounded-lg border border-input bg-background px-3 text-sm"
            >
              <option value="">なし</option>
              {skills.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}（v{s.currentVersion}）
                </option>
              ))}
            </select>
          </div>

          <div className="space-y-1.5">
            <span className="block text-xs font-medium text-foreground/70">ワークフロー（任意）</span>
            <div className="flex flex-wrap items-center gap-1.5">
              {selectedWorkflows.map((w) => (
                <span
                  key={w.id}
                  className="inline-flex items-center gap-1 rounded-full border border-border bg-secondary/60 px-2.5 py-1 text-xs"
                >
                  {w.name}（v{w.currentVersion}）
                  <button
                    type="button"
                    aria-label={`${w.name} を外す`}
                    onClick={() => setSelectedWorkflows((prev) => prev.filter((x) => x.id !== w.id))}
                    className="text-muted-foreground hover:text-destructive"
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
                className="h-9 flex-1 rounded-lg border border-input bg-background px-3 text-sm"
              >
                <option value="">ワークフローを選択</option>
                {workflows
                  .filter((w) => !selectedWorkflows.some((x) => x.id === w.id))
                  .map((w) => (
                    <option key={w.id} value={w.id}>
                      {w.name}（v{w.currentVersion}）
                    </option>
                  ))}
              </select>
              <Button
                type="button"
                size="sm"
                variant="outline"
                disabled={!workflowToAdd}
                onClick={() => {
                  const w = workflows.find((x) => x.id === workflowToAdd);
                  if (w) setSelectedWorkflows((prev) => [...prev, w]);
                  setWorkflowToAdd("");
                }}
              >
                <Plus className="size-4" aria-hidden />
                追加
              </Button>
            </div>
          </div>

          {error ? (
            <p className="rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive" role="alert">
              {error}
            </p>
          ) : null}

          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)} disabled={busy}>
              キャンセル
            </Button>
            <Button type="submit" disabled={busy}>
              {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
              作成
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
