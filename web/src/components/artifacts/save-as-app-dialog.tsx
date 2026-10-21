"use client";

/// 回答の本文の ```ui app を「アプリとして保存」するダイアログ（#604）。
///
/// サーバーが保存済みの回答から図を読み、ボタンの実行先（回答の確定時に解決したワークフロー）ごと
/// ミニアプリにする（`POST /threads/{id}/messages/{id}/save-as-app`）。ピンは保存する人の権限で
/// 検証し直される。

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
import { useMediaQuery } from "@/hooks/use-media-query";
import { toast } from "@/components/ui/use-toast";
import { saveYuiBlockAsApp } from "@/lib/artifact-api";

/// 保存する図（回答のメッセージと図のブロックの識別子・見出し）。
export type SaveAsAppTarget = { threadId: string; messageId: string; block: string; title: string };

export function SaveAsAppDialog({
  open,
  onOpenChange,
  target,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /// 保存する図。
  target: SaveAsAppTarget | null;
}) {
  const router = useRouter();
  const [name, setName] = React.useState("");
  // タッチで開いた瞬間にキーボードを出さない（マウス操作のときだけ自動フォーカス）。
  const finePointer = useMediaQuery("(pointer: fine)");
  const [description, setDescription] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);

  React.useEffect(() => {
    if (open) {
      setName("");
      setDescription(target?.title ?? "");
      setError(null);
    }
  }, [open, target?.title]);

  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    if (busy || !target) return;
    setBusy(true);
    setError(null);
    try {
      const app = await saveYuiBlockAsApp(
        target.threadId,
        target.messageId,
        target.block,
        name.trim(),
        description.trim() || undefined,
      );
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
            この図をボタンの実行先ごと、再利用・共有できるアプリにします。あとで「アプリ」から開けます。
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
              placeholder="expense-app"
              required
              autoFocus={finePointer}
              autoCapitalize="none"
              autoCorrect="off"
              spellCheck={false}
            />
          </div>
          <div className="space-y-1.5">
            <label htmlFor="app-save-desc" className="block text-xs font-medium text-foreground/70">
              画面の見出し
            </label>
            <Input
              id="app-save-desc"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="経費の申請"
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
