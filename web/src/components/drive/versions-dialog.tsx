"use client";

import * as React from "react";
import { Check, Download, History, Loader2, RotateCcw, Sparkles } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { toast } from "@/components/ui/use-toast";
import { useMe } from "@/hooks/use-me";
import { useInfiniteList, useInfiniteSentinel } from "@/hooks/use-infinite-list";
import {
  adoptVersion,
  listVersions,
  restoreVersion,
  versionDownloadUrl,
  type FileVersionResponse,
  type NodeResponse,
} from "@/lib/storage";
import { formatBytes, formatDateTime } from "@/lib/format";

import { LoadingRow } from "./primitives";

/// 版履歴ダイアログ。各版のダウンロード／過去版の復元（新版として非破壊）を提供する。
export function VersionsDialog({
  open,
  onOpenChange,
  node,
  onRestored,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  node: NodeResponse | null;
  /// 版復元で新版が増えたら一覧側を更新する。
  onRestored?: () => void;
}) {
  const me = useMe();
  const fileId = node?.id;
  /// 版作成者の表示名（自分は「自分」、他者は解決名、未解決は subject）。
  const authorLabel = (v: FileVersionResponse): string =>
    me.data?.id && v.author === me.data.id ? "自分" : (v.author_name ?? v.author);
  const fetchPage = React.useCallback(
    (cursor?: string) => {
      if (!fileId) return Promise.resolve({ items: [] as FileVersionResponse[], next_cursor: null });
      return listVersions(fileId, { cursor, limit: 20 });
    },
    [fileId],
  );
  const list = useInfiniteList<FileVersionResponse>(fetchPage, [fileId, open]);
  const sentinelRef = useInfiniteSentinel(list.loadMore, open && list.hasMore && !list.loading);
  const [pending, setPending] = React.useState<number | null>(null);

  const download = async (version: number) => {
    if (!fileId) return;
    try {
      const { url } = await versionDownloadUrl(fileId, version);
      window.open(url, "_blank", "noopener,noreferrer");
    } catch (e) {
      toast({
        variant: "destructive",
        title: "ダウンロードに失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
    }
  };

  /// AI 提案を採用して通常の新バージョンへ昇格する（サーバ側で editor 権限を強制）。
  const adopt = async (version: number) => {
    if (!fileId) return;
    setPending(version);
    try {
      await adoptVersion(fileId, version);
      toast({ title: "採用しました", description: `AI 提案 v${version} を最新版として反映しました` });
      list.reload();
      onRestored?.();
    } catch (e) {
      toast({
        variant: "destructive",
        title: "採用に失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setPending(null);
    }
  };

  const restore = async (version: number) => {
    if (!fileId) return;
    setPending(version);
    try {
      await restoreVersion(fileId, version);
      toast({ title: "復元しました", description: `バージョン ${version} を最新版として復元しました` });
      list.reload();
      onRestored?.();
    } catch (e) {
      toast({
        variant: "destructive",
        title: "復元に失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setPending(null);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <History className="size-5 text-primary" aria-hidden />
            「{node?.name}」の版履歴
          </DialogTitle>
          <DialogDescription>
            過去の版をダウンロード・復元できます。AI 提案は採用すると最新版に反映されます。
          </DialogDescription>
        </DialogHeader>

        <div className="max-h-80 overflow-y-auto rounded-lg border border-border">
          {list.loading ? (
            <LoadingRow />
          ) : list.error ? (
            <p className="px-3 py-6 text-center text-sm text-destructive">{list.error}</p>
          ) : list.items.length === 0 ? (
            <p className="px-3 py-6 text-center text-sm text-muted-foreground">版履歴がありません。</p>
          ) : (
            <ul className="divide-y divide-border">
              {list.items.map((v) => {
                // 提案（is_proposal）は current 未反映のため「最新」は先頭の通常版に付ける。
                const latest = list.items.find((x) => !x.is_proposal)?.version === v.version;
                // 採用済み判定: 同一内容（blob 共有）の新しい通常版が存在する（content-addressing）。
                const adopted =
                  v.is_proposal &&
                  list.items.some(
                    (x) => !x.is_proposal && x.version > v.version && x.blob_sha256 === v.blob_sha256,
                  );
                return (
                  <li key={v.version} className="flex items-center gap-3 px-3 py-2.5">
                    <div className="min-w-0 flex-1">
                      <p className="flex items-center gap-2 text-sm font-medium">
                        バージョン {v.version}
                        {latest ? (
                          <span className="rounded bg-primary/10 px-1.5 py-0.5 text-xs text-primary">最新</span>
                        ) : null}
                        {v.is_proposal ? (
                          <span className="inline-flex items-center gap-1 rounded bg-accent px-1.5 py-0.5 text-xs text-accent-foreground">
                            <Sparkles className="size-3" aria-hidden />
                            AI 提案
                          </span>
                        ) : null}
                      </p>
                      <p className="truncate text-xs text-muted-foreground">
                        {formatDateTime(v.created_at)} · {formatBytes(v.size_bytes)}
                        {v.author ? ` · ${authorLabel(v)}` : ""}
                      </p>
                    </div>
                    <Button type="button" size="sm" variant="ghost" onClick={() => void download(v.version)}>
                      <Download className="size-4" aria-hidden />
                      <span className="sr-only">ダウンロード</span>
                    </Button>
                    {v.is_proposal ? (
                      adopted ? (
                        <span className="inline-flex items-center gap-1 text-xs text-muted-foreground">
                          <Check className="size-3.5" aria-hidden />
                          採用済み
                        </span>
                      ) : (
                        <Button
                          type="button"
                          size="sm"
                          variant="default"
                          // 採用/復元の同時実行を防ぐ（結果が不定になる）。権限はサーバが強制（editor のみ）。
                          disabled={pending !== null}
                          onClick={() => void adopt(v.version)}
                        >
                          {pending === v.version ? (
                            <Loader2 className="size-4 animate-spin" aria-hidden />
                          ) : (
                            <Check className="size-4" aria-hidden />
                          )}
                          採用
                        </Button>
                      )
                    ) : !latest ? (
                      <Button
                        type="button"
                        size="sm"
                        variant="outline"
                        // 復元中は全「復元」ボタンを無効化して同時実行（結果が不定になる）を防ぐ。
                        disabled={pending !== null}
                        onClick={() => void restore(v.version)}
                      >
                        {pending === v.version ? (
                          <Loader2 className="size-4 animate-spin" aria-hidden />
                        ) : (
                          <RotateCcw className="size-4" aria-hidden />
                        )}
                        復元
                      </Button>
                    ) : null}
                  </li>
                );
              })}
              {list.hasMore ? <div ref={sentinelRef}>{list.loadingMore ? <LoadingRow /> : null}</div> : null}
            </ul>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
