"use client";
import * as React from "react";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import { post, type Plan } from "./api";
export function ImportDialog({
  onClose,
  onAccept,
}: {
  onClose: () => void;
  onAccept: (plan: Plan) => void;
}) {
  const [csv, setCsv] = React.useState("");
  const [error, setError] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !busy) onClose();
      }}
    >
      <DialogContent>
        <DialogTitle>CSVインポート</DialogTitle>
        <DialogDescription>
          対象グループの対応付けを置換します。ファイルにないグループは変更しません。
        </DialogDescription>
        <p className="text-xs leading-6 text-muted-foreground">
          ヘッダーは <code>group_id,role_id</code>
          。複数のロールは行を分けて指定し、role_idが空の行は対応解除です。IDはグループ詳細・ロール編集から確認できます。上限は1MB・1,000行です。
        </p>
        <label className="block space-y-2 text-xs font-medium">
          CSVファイル
          <input
            type="file"
            accept=".csv,text/csv"
            disabled={busy}
            className="block w-full rounded-md border p-2 text-xs"
            onChange={async (e) => {
              const file = e.target.files?.[0];
              setError("");
              setCsv("");
              if (!file) return;
              if (file.size > 1_000_000) {
                setError("ファイルは1MB以内にしてください。");
                return;
              }
              try {
                setCsv(await file.text());
              } catch {
                setError("ファイルを読み込めませんでした。");
              }
            }}
          />
        </label>
        <label className="block space-y-2 text-xs font-medium">
          内容を確認・編集
          <textarea
            aria-label="CSVの内容"
            rows={7}
            value={csv}
            onChange={(e) => setCsv(e.target.value)}
            className="w-full rounded-md border bg-background p-3 font-mono text-xs"
            placeholder={"group_id,role_id\nグループID,ロールID"}
          />
        </label>
        {error && (
          <p role="alert" className="text-xs text-destructive">
            {error}
          </p>
        )}
        <div className="flex justify-end gap-2">
          <Button variant="outline" disabled={busy} onClick={onClose}>
            キャンセル
          </Button>
          <Button
            disabled={busy || !csv.trim()}
            onClick={async () => {
              setBusy(true);
              setError("");
              try {
                onAccept(await post<Plan>("/import-plan", { csv }));
              } catch (e) {
                setError((e as Error).message);
              } finally {
                setBusy(false);
              }
            }}
          >
            {busy ? "検証中…" : "検証して確認へ"}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
