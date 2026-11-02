"use client";

/// ミニアプリ管理ページ（Task 6.11）: 一覧・作成・共有・実行画面への導線。

import * as React from "react";
import Link from "next/link";
import { History, LayoutGrid, Loader2, Play, Share2, Trash2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { PageContainer } from "@/components/shell/page-container";
import { ConfirmDialog } from "@/components/drive/dialogs";
import { ArtifactCard } from "@/components/artifacts/artifact-card";
import { toast } from "@/components/ui/use-toast";
import { ArtifactShareDialog } from "@/components/artifacts/share-dialog";
import { MiniAppEditorDialog } from "@/components/artifacts/miniapp-editor";
import { VersionsDialog } from "@/components/artifacts/versions-dialog";
import { ListLoadFailed, ListMore } from "@/components/artifacts/shared-more";
import { deleteArtifact, type ArtifactMeta } from "@/lib/artifact-api";
import { useArtifactList } from "@/hooks/use-artifact-list";
import { InstalledAppsSection } from "@/components/miniapp/installed-apps";
import { savedToast, UNPUBLISHED_SHARE_NOTE } from "@/components/miniapp/publish";
import { useMe } from "@/hooks/use-me";

type DialogState =
  | { kind: "closed" }
  | { kind: "create" }
  | { kind: "share"; meta: ArtifactMeta }
  | { kind: "versions"; meta: ArtifactMeta }
  | { kind: "delete"; meta: ArtifactMeta };

/// カードのバージョンの言い方（D-16）: 共有した人が使うのは公開したバージョン。下書きの番号は持ち主にだけ言う。
function publishLabel(meta: ArtifactMeta, mine: boolean): string {
  const published = meta.publishedVersion;
  if (published == null) return "未公開";
  if (mine && meta.currentVersion > published) return `公開中 v${published}・下書き v${meta.currentVersion}`;
  return `公開中 v${published}`;
}

export default function AppsPage() {
  const list = useArtifactList("mini_app");
  const { items, reload } = list;
  const [dialog, setDialog] = React.useState<DialogState>({ kind: "closed" });
  const [pending, setPending] = React.useState<string | null>(null);
  // 一覧には共有されたアプリも並ぶ。共有（相手の一覧と付与・解除）と削除は持ち主（owner）だけができるので、
  // 自分のアプリにだけ出す（共有を受けた人には一覧が空に見え、付与・削除が 403 になる・BUG-78）。
  // 自分が分かるまでは出さない（実行画面の共有ボタンはサーバの答え `can_share` で出し分ける）。
  const me = useMe();
  const myId = me.data?.id;

  /// 削除は確認ダイアログから（失敗はダイアログ内に出す）。
  const remove = async (meta: ArtifactMeta) => {
    setPending(meta.id);
    try {
      await deleteArtifact(meta.id);
      // 読み込んだ続きを捨てないよう、一覧から外すだけにする（読み直すと先頭のページに戻る）。
      list.remove(meta.id);
      toast({ title: `「${meta.name}」を削除しました` });
    } finally {
      setPending(null);
    }
  };

  const createButton = (
    <Button onClick={() => setDialog({ kind: "create" })}>
      <LayoutGrid className="size-4" aria-hidden />
      アプリを作成
    </Button>
  );

  return (
    <PageContainer
      title="アプリ"
      description="画面とワークフローを束ねたアプリを作り、公開して共有した人に使ってもらえます。"
      className="max-w-4xl"
      actions={createButton}
    >
      {items === null ? (
        <div className="flex items-center justify-center gap-2 py-16 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" aria-hidden />
          読み込み中…
        </div>
      ) : list.error ? (
        <ListLoadFailed what="アプリの一覧" onRetry={reload} />
      ) : items.length === 0 && list.hasMore ? (
        <ListMore noun="アプリ" found={false} list={list} />
      ) : items.length === 0 ? (
        <EmptyState
          icon={LayoutGrid}
          title="アプリはまだありません"
          description="UI スペックとワークフローを束ねて、繰り返し使える画面にしましょう。"
          action={createButton}
        />
      ) : (
        <ul className="grid gap-3 sm:grid-cols-2">
          {items.map((meta) => (
            <ArtifactCard
              key={meta.id}
              name={meta.name}
              meta={`${publishLabel(meta, meta.owner === myId)}・更新 ${new Date(meta.updatedAt).toLocaleDateString("ja-JP")}`}
              pending={pending === meta.id}
              primary={
                <Button size="sm" asChild>
                  <Link href={`/apps/${meta.id}`}>
                    <Play className="size-4" aria-hidden />
                    開く
                  </Link>
                </Button>
              }
              menu={[
                ...(meta.owner === myId
                  ? [
                      { key: "share", label: "共有", icon: Share2, onSelect: () => setDialog({ kind: "share", meta }) },
                      // バージョン履歴は作る人のもの（使う人は公開したバージョンだけを使う・D-16）。共有で編集にした人は
                      // アプリの画面の「バージョン」から開ける。
                      {
                        key: "versions",
                        label: "バージョン履歴",
                        icon: History,
                        onSelect: () => setDialog({ kind: "versions", meta }),
                      },
                    ]
                  : []),
                ...(meta.owner === myId
                  ? [
                      {
                        key: "delete",
                        label: "削除",
                        icon: Trash2,
                        destructive: true,
                        separatorBefore: true,
                        onSelect: () => setDialog({ kind: "delete", meta }),
                      },
                    ]
                  : []),
              ]}
            />
          ))}
        </ul>
      )}
      {items && items.length > 0 ? <ListMore noun="アプリ" found list={list} /> : null}

      <InstalledAppsSection />

      <MiniAppEditorDialog
        open={dialog.kind === "create"}
        onOpenChange={(open) => !open && setDialog({ kind: "closed" })}
        onSaved={(saved) => {
          reload();
          // 次に起きること（公開すると使える・書き損じがあれば直すまで公開できない）はアプリの画面の保存と同じ文。
          toast({ ...savedToast(saved), title: "作成しました" });
        }}
      />
      <ArtifactShareDialog
        open={dialog.kind === "share"}
        onOpenChange={(open) => !open && setDialog({ kind: "closed" })}
        artifactId={dialog.kind === "share" ? dialog.meta.id : null}
        name={dialog.kind === "share" ? dialog.meta.name : ""}
        note={dialog.kind === "share" && dialog.meta.publishedVersion == null ? UNPUBLISHED_SHARE_NOTE : undefined}
      />
      <VersionsDialog
        open={dialog.kind === "versions"}
        onOpenChange={(open) => !open && setDialog({ kind: "closed" })}
        artifactId={dialog.kind === "versions" ? dialog.meta.id : null}
        name={dialog.kind === "versions" ? dialog.meta.name : ""}
      />
      <ConfirmDialog
        open={dialog.kind === "delete"}
        onOpenChange={(open) => !open && setDialog({ kind: "closed" })}
        title="アプリを削除しますか？"
        description={
          dialog.kind === "delete"
            ? `「${dialog.meta.name}」を削除します。バージョン履歴は保持されます。`
            : undefined
        }
        confirmLabel="削除する"
        destructive
        onConfirm={async () => {
          if (dialog.kind === "delete") await remove(dialog.meta);
        }}
      />
    </PageContainer>
  );
}
