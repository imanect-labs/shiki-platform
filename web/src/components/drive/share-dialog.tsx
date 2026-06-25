"use client";

import * as React from "react";
import { Loader2, Search, UserPlus, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { toast } from "@/components/ui/use-toast";
import {
  listShares,
  searchDirectory,
  shareNode,
  unshareNode,
  type DirectoryUserResponse,
  type NodeResponse,
  type ShareEntry,
  type ShareRole,
} from "@/lib/storage";
import { cn } from "@/lib/utils";

const ROLES: { value: ShareRole; label: string }[] = [
  { value: "viewer", label: "閲覧" },
  { value: "editor", label: "編集" },
];

/// 共有ダイアログ。同テナントのユーザーを検索して個人へ閲覧/編集権限を付与する。
/// 別テナントのユーザーは検索結果に出ない（サーバ側 tenant_id スコープ）。
/// 部署/グループ共有は #76（SAAS.2）で延期。
export function ShareDialog({
  open,
  onOpenChange,
  node,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  node: NodeResponse | null;
}) {
  const [query, setQuery] = React.useState("");
  const [results, setResults] = React.useState<DirectoryUserResponse[]>([]);
  const [searching, setSearching] = React.useState(false);
  const [role, setRole] = React.useState<ShareRole>("viewer");
  const [shares, setShares] = React.useState<ShareEntry[]>([]);
  const [loadingShares, setLoadingShares] = React.useState(false);
  const [pendingId, setPendingId] = React.useState<string | null>(null);

  // 開いたら状態リセット＋現在の共有相手を読む。
  React.useEffect(() => {
    if (!open || !node) return;
    setQuery("");
    setResults([]);
    setRole("viewer");
    setLoadingShares(true);
    listShares(node.id)
      .then(setShares)
      .catch(() => setShares([]))
      .finally(() => setLoadingShares(false));
  }, [open, node]);

  // インクリメンタル検索（デバウンス。全件取得はせず先頭ページのみ）。
  React.useEffect(() => {
    if (!open) return;
    const handle = setTimeout(() => {
      setSearching(true);
      searchDirectory({ q: query, limit: 8 })
        .then((res) => setResults(res.items))
        .catch(() => setResults([]))
        .finally(() => setSearching(false));
    }, 200);
    return () => clearTimeout(handle);
  }, [open, query]);

  const sharedUserIds = React.useMemo(
    () => new Set(shares.map((s) => s.target.id)),
    [shares],
  );

  if (!node) return null;

  const grant = async (user: DirectoryUserResponse) => {
    setPendingId(user.id);
    try {
      await shareNode(node.id, user.id, role);
      const next = await listShares(node.id);
      setShares(next);
      toast({ title: "共有しました", description: `${user.display_name} に${role === "editor" ? "編集" : "閲覧"}権限を付与` });
    } catch (e) {
      toast({
        variant: "destructive",
        title: "共有に失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setPendingId(null);
    }
  };

  const revoke = async (entry: ShareEntry) => {
    setPendingId(entry.target.id);
    try {
      await unshareNode(node.id, entry.target.id, entry.role);
      setShares((prev) => prev.filter((s) => s.target.id !== entry.target.id));
    } catch (e) {
      toast({
        variant: "destructive",
        title: "解除に失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setPendingId(null);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>「{node.name}」を共有</DialogTitle>
          <DialogDescription>
            同じ組織のメンバーを検索して権限を付与します（個人のみ・部署共有は近日）。
          </DialogDescription>
        </DialogHeader>

        {/* 役割の選択 */}
        <div className="flex items-center gap-2">
          <span className="text-sm text-muted-foreground">付与する権限</span>
          <div className="inline-flex rounded-md border border-border p-0.5">
            {ROLES.map((r) => (
              <button
                key={r.value}
                type="button"
                onClick={() => setRole(r.value)}
                className={cn(
                  "rounded px-3 py-1 text-sm transition-colors",
                  role === r.value
                    ? "bg-primary text-primary-foreground"
                    : "text-muted-foreground hover:text-foreground",
                )}
              >
                {r.label}
              </button>
            ))}
          </div>
        </div>

        {/* 検索 */}
        <div className="relative">
          <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" aria-hidden />
          <Input
            autoFocus
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="名前・メールで検索"
            className="pl-9"
          />
        </div>

        {/* 検索結果 */}
        <div className="max-h-48 overflow-y-auto rounded-lg border border-border">
          {searching ? (
            <div className="flex items-center justify-center gap-2 py-5 text-sm text-muted-foreground">
              <Loader2 className="size-4 animate-spin" aria-hidden />
              検索中…
            </div>
          ) : results.length === 0 ? (
            <p className="px-3 py-5 text-center text-sm text-muted-foreground">
              該当するユーザーがいません
            </p>
          ) : (
            <ul className="divide-y divide-border">
              {results.map((u) => {
                const already = sharedUserIds.has(u.id);
                return (
                  <li key={u.id} className="flex items-center gap-3 px-3 py-2">
                    <div className="min-w-0 flex-1">
                      <p className="truncate text-sm font-medium">{u.display_name}</p>
                      <p className="truncate text-xs text-muted-foreground">{u.email}</p>
                    </div>
                    <Button
                      type="button"
                      size="sm"
                      variant={already ? "ghost" : "outline"}
                      disabled={already || pendingId === u.id}
                      onClick={() => void grant(u)}
                    >
                      {pendingId === u.id ? (
                        <Loader2 className="size-4 animate-spin" aria-hidden />
                      ) : (
                        <UserPlus className="size-4" aria-hidden />
                      )}
                      {already ? "共有済み" : "共有"}
                    </Button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>

        {/* 現在の共有相手 */}
        <div>
          <p className="mb-2 text-sm font-medium">共有中のメンバー</p>
          {loadingShares ? (
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <Loader2 className="size-4 animate-spin" aria-hidden />
              読み込み中…
            </div>
          ) : shares.length === 0 ? (
            <p className="text-sm text-muted-foreground">まだ誰にも共有していません。</p>
          ) : (
            <ul className="flex flex-col gap-1">
              {shares.map((s) => (
                <li
                  key={`${s.target.id}-${s.role}`}
                  className="flex items-center gap-3 rounded-md border border-border px-3 py-2"
                >
                  <span className="min-w-0 flex-1 truncate text-sm">{s.target.id}</span>
                  <span className="rounded bg-secondary px-2 py-0.5 text-xs text-secondary-foreground">
                    {s.role === "editor" ? "編集" : "閲覧"}
                  </span>
                  <button
                    type="button"
                    aria-label="共有を解除"
                    disabled={pendingId === s.target.id}
                    onClick={() => void revoke(s)}
                    className="rounded p-1 text-muted-foreground transition-colors hover:bg-destructive/10 hover:text-destructive"
                  >
                    {pendingId === s.target.id ? (
                      <Loader2 className="size-4 animate-spin" aria-hidden />
                    ) : (
                      <X className="size-4" aria-hidden />
                    )}
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
