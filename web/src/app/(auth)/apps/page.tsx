"use client";

/// ミニアプリ管理ページ（Task 6.11）: 一覧・作成・共有・実行画面への導線。

import * as React from "react";
import Link from "next/link";
import { History, LayoutGrid, Loader2, Play, Share2, Trash2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { toast } from "@/components/ui/use-toast";
import { ArtifactShareDialog } from "@/components/artifacts/share-dialog";
import { MiniAppEditorDialog } from "@/components/artifacts/miniapp-editor";
import { VersionsDialog } from "@/components/artifacts/versions-dialog";
import { deleteArtifact, listArtifacts, type ArtifactMeta } from "@/lib/artifact-api";

type DialogState =
  | { kind: "closed" }
  | { kind: "create" }
  | { kind: "share"; meta: ArtifactMeta }
  | { kind: "versions"; meta: ArtifactMeta };

export default function AppsPage() {
  const [items, setItems] = React.useState<ArtifactMeta[] | null>(null);
  const [dialog, setDialog] = React.useState<DialogState>({ kind: "closed" });
  const [pending, setPending] = React.useState<string | null>(null);

  const reload = React.useCallback(() => {
    listArtifacts("mini_app")
      .then(setItems)
      .catch(() => setItems([]));
  }, []);
  React.useEffect(reload, [reload]);

  const remove = async (meta: ArtifactMeta) => {
    if (!window.confirm(`アプリ「${meta.name}」を削除しますか？（バージョン履歴は保持されます）`)) return;
    setPending(meta.id);
    try {
      await deleteArtifact(meta.id);
      reload();
    } catch (e) {
      toast({
        variant: "destructive",
        title: "削除に失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setPending(null);
    }
  };

  return (
    <div className="mx-auto w-full max-w-4xl px-4 py-6">
      <div className="mb-5 flex items-center justify-between">
        <div>
          <h1 className="text-lg font-semibold">アプリ</h1>
          <p className="text-sm text-muted-foreground">
            スキル・UI・ワークフローを束ねたミニアプリを共有して実行できます。
          </p>
        </div>
        <Button onClick={() => setDialog({ kind: "create" })}>
          <LayoutGrid className="size-4" aria-hidden />
          アプリを作成
        </Button>
      </div>

      {items === null ? (
        <div className="flex items-center justify-center gap-2 py-16 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" aria-hidden />
          読み込み中…
        </div>
      ) : items.length === 0 ? (
        <EmptyState
          icon={LayoutGrid}
          title="アプリはまだありません"
          description="「アプリを作成」から、UI スペックとワークフローを束ねましょう。"
        />
      ) : (
        <ul className="grid gap-3 sm:grid-cols-2">
          {items.map((meta) => (
            <li
              key={meta.id}
              className="group flex flex-col gap-2 rounded-xl border border-border bg-card p-4 transition-shadow hover:shadow-[var(--elevation-1,0_1px_3px_rgb(0_0_0/0.08))]"
            >
              <div className="flex items-start justify-between gap-2">
                <div className="min-w-0">
                  <h2 className="truncate text-[15px] font-semibold">{meta.name}</h2>
                  <p className="text-xs text-muted-foreground">
                    v{meta.currentVersion}・更新 {new Date(meta.updatedAt).toLocaleDateString("ja-JP")}
                  </p>
                </div>
                {pending === meta.id ? (
                  <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" aria-hidden />
                ) : null}
              </div>
              <div className="mt-auto flex flex-wrap items-center gap-1 pt-1">
                <Button size="sm" asChild>
                  <Link href={`/apps/${meta.id}`}>
                    <Play className="size-4" aria-hidden />
                    開く
                  </Link>
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  aria-label={`${meta.name} を共有`}
                  onClick={() => setDialog({ kind: "share", meta })}
                >
                  <Share2 className="size-4" aria-hidden />
                  共有
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  aria-label={`${meta.name} のバージョン履歴`}
                  onClick={() => setDialog({ kind: "versions", meta })}
                >
                  <History className="size-4" aria-hidden />
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  aria-label={`${meta.name} を削除`}
                  onClick={() => void remove(meta)}
                  className="text-muted-foreground hover:text-destructive"
                >
                  <Trash2 className="size-4" aria-hidden />
                </Button>
              </div>
            </li>
          ))}
        </ul>
      )}

      <MiniAppEditorDialog
        open={dialog.kind === "create"}
        onOpenChange={(open) => !open && setDialog({ kind: "closed" })}
        onSaved={() => {
          reload();
          toast({ title: "作成しました" });
        }}
      />
      <ArtifactShareDialog
        open={dialog.kind === "share"}
        onOpenChange={(open) => !open && setDialog({ kind: "closed" })}
        artifactId={dialog.kind === "share" ? dialog.meta.id : null}
        name={dialog.kind === "share" ? dialog.meta.name : ""}
      />
      <VersionsDialog
        open={dialog.kind === "versions"}
        onOpenChange={(open) => !open && setDialog({ kind: "closed" })}
        artifactId={dialog.kind === "versions" ? dialog.meta.id : null}
        name={dialog.kind === "versions" ? dialog.meta.name : ""}
      />
    </div>
  );
}
