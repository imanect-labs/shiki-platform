"use client";

/// チャットで生成した generative UI を「アプリとして保存」するダイアログ（Phase 6 UX）。
///
/// 手順: 検証済みスペックを `POST /ui-specs` で保存 → それをピンした `POST /mini-apps` を作る。
/// スペックに含まれる workflow 束縛は検証時にピン済みなので、そのままバンドルの workflows へ入れる
/// （`check_bindings_subset` を通る）。chat.submit（handler 束縛）を含むスペックはミニアプリに
/// できない（保存時 422）ため、呼び出し側がボタンを非活性にする。

import * as React from "react";
import { useRouter } from "next/navigation";
import { Loader2 } from "lucide-react";

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
import { toast } from "@/components/ui/use-toast";
import { createMiniApp, createUiSpec } from "@/lib/artifact-api";
import type { MiniAppBody } from "@/generated/gui-spec";

/// スペックからバンドルに載せる workflow ピンを抽出する（既に version 焼き込み済み）。
function workflowPinsFrom(spec: unknown): MiniAppBody["workflows"] {
  if (typeof spec !== "object" || spec === null) return [];
  const actions = (spec as { actions?: unknown }).actions;
  if (!Array.isArray(actions)) return [];
  const pins: MiniAppBody["workflows"] = [];
  const seen = new Set<string>();
  for (const a of actions) {
    if (a && typeof a === "object" && (a as { type?: string }).type === "workflow") {
      const wf = (a as { workflow?: { name?: string; artifact_id?: string; version?: number } })
        .workflow;
      if (wf?.artifact_id != null && wf.version != null) {
        // 同一 workflow（同 id＋version）を複数ボタンが起動しても重複ピンを作らない。
        const key = `${wf.artifact_id}@${wf.version}`;
        if (seen.has(key)) continue;
        seen.add(key);
        pins.push({ alias: wf.name ?? wf.artifact_id, artifact_id: wf.artifact_id, version: wf.version });
      }
    }
  }
  return pins;
}

export function SaveAsAppDialog({
  open,
  onOpenChange,
  spec,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /// 保存対象の検証済みスペック（generative_ui ブロックの spec）。
  spec: unknown;
}) {
  const router = useRouter();
  const [name, setName] = React.useState("");
  const [description, setDescription] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);

  React.useEffect(() => {
    if (open) {
      setName("");
      setDescription("");
      setError(null);
    }
  }, [open]);

  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      const created = await createUiSpec(name.trim(), spec);
      const body: MiniAppBody = {
        description: description.trim() || name.trim(),
        ui_spec: { artifact_id: created.id, version: created.version },
        skill: null,
        workflows: workflowPinsFrom(spec),
      };
      const app = await createMiniApp(name.trim(), body);
      toast({ title: "アプリとして保存しました" });
      onOpenChange(false);
      router.push(`/apps/${app.id}`);
    } catch (err) {
      setError(err instanceof Error ? err.message : "保存に失敗しました");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>アプリとして保存</DialogTitle>
          <DialogDescription>
            この画面を再利用・共有できるアプリにします。あとで「アプリ」から開けます。
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={save} className="space-y-4">
          <div className="space-y-1.5">
            <label htmlFor="app-save-name" className="block text-xs font-medium text-foreground/70">
              名前<span className="ml-0.5 text-destructive">*</span>
            </label>
            <Input
              id="app-save-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="月次レポート"
              required
              autoFocus
            />
          </div>
          <div className="space-y-1.5">
            <label htmlFor="app-save-desc" className="block text-xs font-medium text-foreground/70">
              説明（任意）
            </label>
            <Input
              id="app-save-desc"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="四半期の部門別サマリ"
            />
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
            <Button type="submit" disabled={busy || !name.trim()}>
              {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
              保存
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/// スペックが chat.submit（handler 束縛）を含むか＝ミニアプリにできないか。
export function specHasChatOnlyAction(spec: unknown): boolean {
  if (typeof spec !== "object" || spec === null) return false;
  const actions = (spec as { actions?: unknown }).actions;
  if (!Array.isArray(actions)) return false;
  return actions.some((a) => a && typeof a === "object" && (a as { type?: string }).type === "handler");
}
