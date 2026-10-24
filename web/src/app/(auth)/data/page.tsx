"use client";

/// 業務データ（構造化データ）のテーブル一覧（Task 15.5）: 自分が使えるテーブル（owner・共有された）と作成。

import { Database, Loader2, Plus, Table2, Trash2 } from "lucide-react";
import { useRouter } from "next/navigation";
import * as React from "react";

import { ConfirmDialog } from "@/components/drive/dialogs";
import { PageContainer } from "@/components/shell/page-container";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { EmptyState } from "@/components/ui/empty-state";
import { Input } from "@/components/ui/input";
import { toast } from "@/components/ui/use-toast";
import { useMe } from "@/hooks/use-me";
import { createTable, DataApiError, deleteTable, listTables, type DataTable } from "@/lib/data-api";
import { keyFromLabel } from "@/lib/data-view";
import { cn } from "@/lib/utils";

const dateFmt = new Intl.DateTimeFormat("ja-JP", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" });

function CreateTableDialog({
  open,
  onOpenChange,
  onCreated,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  onCreated: (t: DataTable) => void;
}) {
  const [name, setName] = React.useState("");
  const [first, setFirst] = React.useState("名前");
  const [busy, setBusy] = React.useState(false);
  const [nameError, setNameError] = React.useState<string | null>(null);
  React.useEffect(() => {
    if (open) {
      setName("");
      setFirst("名前");
      setNameError(null);
    }
  }, [open]);
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!name.trim()) return;
    setBusy(true);
    try {
      const label = first.trim() || "名前";
      const t = await createTable(name.trim(), {
        // 最初の列は索引付き（既定の並べ替え・絞り込みにすぐ使える）。
        fields: [{ key: keyFromLabel(label, new Set()), display_name: label, type: "text", indexed: true }],
        field_policy: [],
      });
      onCreated(t);
    } catch (err) {
      // 同じ名前（409 name_taken・旧サーバは 409 conflict）は名前の欄の下に出す（A-2）。
      if (err instanceof DataApiError && (err.nameTaken || (err.status === 409 && err.code === "conflict"))) {
        setNameError("同じ名前のテーブルがあります。別の名前にしてください。");
        return;
      }
      if (err instanceof DataApiError && err.status === 400 && err.field === "name") {
        setNameError(err.message);
        return;
      }
      toast({
        variant: "destructive",
        title: "テーブルを作れませんでした",
        description: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-sm">
        <form onSubmit={submit} className="space-y-4">
          <DialogHeader>
            <DialogTitle>テーブルを作成</DialogTitle>
            <DialogDescription>列はあとから「列の編集」で足せます。</DialogDescription>
          </DialogHeader>
          <div className="space-y-3">
            <div className="space-y-1.5">
              <label htmlFor="new-table-name" className="text-xs font-medium">
                テーブル名
              </label>
              <Input
                id="new-table-name"
                autoFocus
                maxLength={128}
                value={name}
                onChange={(e) => {
                  setName(e.target.value);
                  setNameError(null);
                }}
                placeholder="例: 経費精算"
                aria-invalid={nameError ? true : undefined}
                aria-describedby={nameError ? "new-table-name-error" : undefined}
              />
              {nameError ? (
                <p id="new-table-name-error" role="alert" className="text-xs text-destructive">
                  {nameError}
                </p>
              ) : null}
            </div>
            <div className="space-y-1.5">
              <label htmlFor="new-table-first" className="text-xs font-medium">
                最初の列
              </label>
              <Input id="new-table-first" maxLength={100} value={first} onChange={(e) => setFirst(e.target.value)} />
            </div>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              キャンセル
            </Button>
            <Button type="submit" disabled={busy || !name.trim()} data-testid="create-table-submit">
              {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
              作成
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

export default function DataTablesPage() {
  const router = useRouter();
  const me = useMe();
  const [items, setItems] = React.useState<DataTable[] | null>(null);
  const [creating, setCreating] = React.useState(false);
  const [deleting, setDeleting] = React.useState<DataTable | null>(null);

  React.useEffect(() => {
    listTables()
      .then((ts) => setItems([...ts].sort((a, b) => b.updated_at.localeCompare(a.updated_at))))
      .catch((e) => {
        setItems([]);
        toast({
          variant: "destructive",
          title: "一覧の取得に失敗しました",
          description: e instanceof Error ? e.message : String(e),
        });
      });
  }, []);

  const createButton = (
    <Button onClick={() => setCreating(true)} data-testid="create-table">
      <Plus className="size-4" aria-hidden />
      <span className="sm:hidden">新規作成</span>
      <span className="hidden sm:inline">新しいテーブル</span>
    </Button>
  );

  return (
    <PageContainer
      title="データ"
      description="業務データのテーブル。行と列は開いた人の権限で絞られます。"
      className="max-w-4xl"
      actions={createButton}
    >
      {items === null ? (
        <div className="flex items-center justify-center gap-2 py-16 text-sm text-muted-foreground" role="status">
          <Loader2 className="size-4 animate-spin" aria-hidden />
          読み込み中…
        </div>
      ) : items.length === 0 ? (
        <EmptyState
          seasonal
          icon={Database}
          title="まだテーブルがありません"
          description="表で入力・絞り込み・並べ替えができます。"
          action={
            <Button variant="outline" onClick={() => setCreating(true)}>
              <Plus className="size-4" aria-hidden />
              テーブルを作成
            </Button>
          }
        />
      ) : (
        <ul className="rule-soft overflow-hidden rounded-xl border bg-card" data-testid="data-table-list">
          {items.map((t, i) => (
            <li key={t.id} className={cn("group/row relative flex items-center", i > 0 && "shiki-dash-top")}>
              <button
                type="button"
                onClick={() => router.push(`/data/${t.id}`)}
                className="flex w-full min-w-0 flex-1 items-center gap-4 px-4 py-3.5 text-left transition-colors duration-fast hover:bg-accent/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
              >
                <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary">
                  <Table2 className="size-4.5" aria-hidden />
                </span>
                <span className="min-w-0 flex-1">
                  <span className="flex items-center gap-2">
                    <span className="truncate text-sm font-medium">{t.name}</span>
                    {t.created_by !== me.data?.id ? <Badge variant="muted">共有</Badge> : null}
                    {t.app_id ? <Badge variant="outline">アプリ</Badge> : null}
                  </span>
                  <span className="mt-0.5 block truncate text-xs text-muted-foreground">
                    {t.schema.fields.length} 列 · {dateFmt.format(new Date(t.updated_at))} 更新
                  </span>
                </span>
              </button>
              {/* 削除はテーブルの owner（作成者）だけ（サーバも owner 以外を 403 にする）。アプリのテーブルはアプリ側で消す。 */}
              {t.created_by === me.data?.id && !t.app_id ? (
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label={`「${t.name}」を削除`}
                  // マウスではホバーで出す。タッチ端末には hover が無いので常に出す。
                  className="mr-2 shrink-0 text-muted-foreground transition-opacity duration-fast hover:text-destructive focus-visible:opacity-100 pointer-fine:opacity-0 pointer-fine:group-hover/row:opacity-100"
                  onClick={() => setDeleting(t)}
                >
                  <Trash2 className="size-4" aria-hidden />
                </Button>
              ) : null}
            </li>
          ))}
        </ul>
      )}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(o) => !o && setDeleting(null)}
        title="テーブルを削除しますか？"
        description={
          deleting
            ? `「${deleting.name}」とそのレコード・ビューを削除します。元に戻せません。このテーブルを使うワークフローやアプリは読めなくなります。`
            : undefined
        }
        confirmLabel="削除する"
        destructive
        onConfirm={async () => {
          if (!deleting) return;
          await deleteTable(deleting.id);
          setItems((xs) => (xs ?? []).filter((x) => x.id !== deleting.id));
          toast({ title: `「${deleting.name}」を削除しました` });
        }}
      />
      <CreateTableDialog
        open={creating}
        onOpenChange={setCreating}
        onCreated={(t) => {
          setCreating(false);
          router.push(`/data/${t.id}`);
        }}
      />
    </PageContainer>
  );
}
