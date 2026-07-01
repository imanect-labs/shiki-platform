"use client";

import * as React from "react";
import { ChevronRight, Home, Loader2 } from "lucide-react";

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
import { listChildren, type NodeResponse } from "@/lib/storage";
import { cn } from "@/lib/utils";
import { NodeIcon } from "./primitives";

/// テキスト 1 入力のダイアログ（新規フォルダ / リネーム共用）。
export function TextPromptDialog({
  open,
  onOpenChange,
  title,
  description,
  label,
  initialValue = "",
  submitLabel,
  onSubmit,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description?: string;
  label: string;
  initialValue?: string;
  submitLabel: string;
  onSubmit: (value: string) => Promise<void>;
}) {
  const [value, setValue] = React.useState(initialValue);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);

  // 開くたびに初期値へ戻す。
  React.useEffect(() => {
    if (open) {
      setValue(initialValue);
      setError(null);
      setBusy(false);
    }
  }, [open, initialValue]);

  const submit = async () => {
    const trimmed = value.trim();
    if (!trimmed) {
      setError("名前を入力してください");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await onSubmit(trimmed);
      onOpenChange(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          {description ? <DialogDescription>{description}</DialogDescription> : null}
        </DialogHeader>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
          className="flex flex-col gap-3"
        >
          <label className="flex flex-col gap-1.5 text-sm">
            <span className="font-medium">{label}</span>
            <Input
              autoFocus
              value={value}
              onChange={(e) => setValue(e.target.value)}
              aria-invalid={error ? true : undefined}
            />
          </label>
          {error ? <p className="text-sm text-destructive">{error}</p> : null}
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)} disabled={busy}>
              キャンセル
            </Button>
            <Button type="submit" disabled={busy}>
              {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
              {submitLabel}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/// 汎用の確認ダイアログ（削除など）。
export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  confirmLabel,
  destructive,
  onConfirm,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description?: string;
  confirmLabel: string;
  destructive?: boolean;
  onConfirm: () => Promise<void>;
}) {
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);

  React.useEffect(() => {
    if (open) {
      setBusy(false);
      setError(null);
    }
  }, [open]);

  const confirm = async () => {
    setBusy(true);
    setError(null);
    try {
      await onConfirm();
      onOpenChange(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          {description ? <DialogDescription>{description}</DialogDescription> : null}
        </DialogHeader>
        {error ? <p className="text-sm text-destructive">{error}</p> : null}
        <DialogFooter>
          <Button type="button" variant="ghost" onClick={() => onOpenChange(false)} disabled={busy}>
            キャンセル
          </Button>
          <Button
            type="button"
            variant={destructive ? "destructive" : "default"}
            onClick={() => void confirm()}
            disabled={busy}
          >
            {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
            {confirmLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/// 移動先フォルダを選ぶダイアログ。フォルダ階層をたどって「ここへ移動」する。
/// 移動対象（とその子孫）は循環を避けるため選択不可（サーバも 400 で拒否）。
export function MoveDialog({
  open,
  onOpenChange,
  node,
  onMove,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  node: NodeResponse | null;
  /// `null` でルートへ移動。
  onMove: (destinationId: string | null) => Promise<void>;
}) {
  const [folderId, setFolderId] = React.useState<string | null>(null);
  const [folderName, setFolderName] = React.useState<string>("ドライブ");
  const [folders, setFolders] = React.useState<NodeResponse[]>([]);
  const [cursor, setCursor] = React.useState<string | undefined>(undefined);
  const [hasMore, setHasMore] = React.useState(false);
  const [loading, setLoading] = React.useState(false);
  const [loadingMore, setLoadingMore] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);

  // 開いたらルートから。
  React.useEffect(() => {
    if (open) {
      setFolderId(null);
      setFolderName("ドライブ");
      setError(null);
      setBusy(false);
    }
  }, [open]);

  // 現在地のサブフォルダを先頭ページから読み込む（フォルダのみ表示）。
  React.useEffect(() => {
    if (!open) return;
    let active = true;
    setLoading(true);
    setFolders([]);
    setCursor(undefined);
    setHasMore(false);
    listChildren({ parentId: folderId ?? undefined, sort: "name", limit: 100 })
      .then((page) => {
        if (!active) return;
        setFolders(page.items.filter((n) => n.kind === "folder"));
        setCursor(page.next_cursor ?? undefined);
        setHasMore(Boolean(page.next_cursor));
      })
      .catch((e: unknown) => {
        if (active) setError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, [open, folderId]);

  // 続きのページを読み込み、フォルダを追記する（>100 件でも移動先に辿り着ける）。
  const loadMoreFolders = async () => {
    if (!cursor || loadingMore) return;
    setLoadingMore(true);
    try {
      const page = await listChildren({
        parentId: folderId ?? undefined,
        sort: "name",
        cursor,
        limit: 100,
      });
      setFolders((prev) => [...prev, ...page.items.filter((n) => n.kind === "folder")]);
      setCursor(page.next_cursor ?? undefined);
      setHasMore(Boolean(page.next_cursor));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoadingMore(false);
    }
  };

  const move = async () => {
    setBusy(true);
    setError(null);
    try {
      await onMove(folderId);
      onOpenChange(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  // 移動対象自身の中へは入れられない（子孫への移動はサーバが 400 で弾く）。
  const sameAsSource = node?.kind === "folder" && folderId === node.id;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>{node ? `「${node.name}」を移動` : "移動"}</DialogTitle>
          <DialogDescription>移動先のフォルダを選んでください。</DialogDescription>
        </DialogHeader>

        <div className="flex items-center gap-1.5 text-sm text-muted-foreground">
          <Home className="size-4" aria-hidden />
          <span className="truncate">{folderName}</span>
        </div>

        <div className="max-h-[55vh] min-h-[280px] overflow-y-auto rounded-lg border border-border">
          {loading ? (
            <div className="flex items-center justify-center gap-2 py-6 text-sm text-muted-foreground">
              <Loader2 className="size-4 animate-spin" aria-hidden />
              読み込み中…
            </div>
          ) : folders.length === 0 && !hasMore ? (
            <p className="px-3 py-6 text-center text-sm text-muted-foreground">サブフォルダはありません</p>
          ) : (
            <ul className="divide-y divide-border">
              {folders.map((f) => {
                const disabled = node?.kind === "folder" && f.id === node.id;
                return (
                  <li key={f.id}>
                    <button
                      type="button"
                      disabled={disabled}
                      onClick={() => {
                        setFolderId(f.id);
                        setFolderName(f.name);
                      }}
                      className={cn(
                        "flex w-full items-center gap-3 px-4 py-3 text-left text-[15px] transition-colors hover:bg-accent",
                        disabled && "cursor-not-allowed opacity-40 hover:bg-transparent",
                      )}
                    >
                      <NodeIcon kind="folder" className="size-6 shrink-0" />
                      <span className="flex-1 truncate">{f.name}</span>
                      <ChevronRight className="size-4 shrink-0 text-muted-foreground" aria-hidden />
                    </button>
                  </li>
                );
              })}
              {hasMore ? (
                <li>
                  <button
                    type="button"
                    onClick={() => void loadMoreFolders()}
                    disabled={loadingMore}
                    className="flex w-full items-center justify-center gap-2 px-3 py-2 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                  >
                    {loadingMore ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
                    もっと読み込む
                  </button>
                </li>
              ) : null}
            </ul>
          )}
        </div>

        {error ? <p className="text-sm text-destructive">{error}</p> : null}

        <DialogFooter>
          <Button type="button" variant="ghost" onClick={() => onOpenChange(false)} disabled={busy}>
            キャンセル
          </Button>
          <Button type="button" onClick={() => void move()} disabled={busy || sameAsSource}>
            {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
            ここへ移動
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
