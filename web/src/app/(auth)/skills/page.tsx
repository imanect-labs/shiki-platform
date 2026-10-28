"use client";

/// スキル管理ページ（Task 6.11）: 一覧・共有・バージョン履歴。作成・編集はスキルエディタ
/// （`/skills/new`・`/skills/{id}`・#919）で行う。

import * as React from "react";
import { useRouter } from "next/navigation";
import { History, Loader2, MessageSquareText, Pencil, Share2, Sparkles, Store, Trash2, UploadCloud } from "lucide-react";

import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { PageContainer } from "@/components/shell/page-container";
import { ConfirmDialog } from "@/components/drive/dialogs";
import { ArtifactCard } from "@/components/artifacts/artifact-card";
import { toast } from "@/components/ui/use-toast";
import { ArtifactShareDialog } from "@/components/artifacts/share-dialog";
import { VersionsDialog } from "@/components/artifacts/versions-dialog";
import { deleteArtifact, listArtifacts, type ArtifactMeta } from "@/lib/artifact-api";
import { createThread } from "@/lib/chat-api";
import { publishSkill } from "@/lib/skill-registry-api";
import { SkillStoreSection } from "@/components/artifacts/skill-store-section";

type DialogState =
  | { kind: "closed" }
  | { kind: "share"; meta: ArtifactMeta }
  | { kind: "versions"; meta: ArtifactMeta }
  | { kind: "delete"; meta: ArtifactMeta };

export default function SkillsPage() {
  const router = useRouter();
  const [items, setItems] = React.useState<ArtifactMeta[] | null>(null);
  const [dialog, setDialog] = React.useState<DialogState>({ kind: "closed" });
  const [pending, setPending] = React.useState<string | null>(null);

  const reload = React.useCallback(() => {
    listArtifacts("skill")
      .then(setItems)
      .catch(() => setItems([]));
  }, []);
  React.useEffect(reload, [reload]);

  const startChat = async (meta: ArtifactMeta) => {
    setPending(meta.id);
    try {
      const thread = await createThread(meta.name, false, {
        skill: { artifactId: meta.id, version: meta.currentVersion },
      });
      router.push(`/c/${thread.id}`);
    } catch (e) {
      toast({
        variant: "destructive",
        title: "チャットを開始できませんでした",
        description: e instanceof Error ? e.message : String(e),
      });
      setPending(null);
    }
  };

  const publish = async (meta: ArtifactMeta) => {
    setPending(meta.id);
    try {
      await publishSkill(meta.id, String(meta.currentVersion));
      toast({
        title: `「${meta.name}」を公開しました`,
        description: "スキルストアからインストールできます。",
      });
    } catch (e) {
      toast({
        variant: "destructive",
        title: "公開に失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setPending(null);
    }
  };

  /// 削除は確認ダイアログから（失敗はダイアログ内に出す）。
  const remove = async (meta: ArtifactMeta) => {
    setPending(meta.id);
    try {
      await deleteArtifact(meta.id);
      reload();
      toast({ title: `「${meta.name}」を削除しました` });
    } finally {
      setPending(null);
    }
  };

  const scrollToStore = () => {
    const store = document.getElementById("skill-store");
    if (store) store.scrollIntoView({ behavior: "smooth", block: "start" });
    else toast({ title: "ストアに公開されたスキルはまだありません" });
  };

  return (
    <PageContainer
      title="スキル"
      description="指示文・知識スコープ・許可ツールをまとめて、チャットに適用できます。"
      className="max-w-4xl"
      actions={
        <Button onClick={() => router.push("/skills/new")}>
          <Sparkles className="size-4" aria-hidden />
          スキルを作成
        </Button>
      }
    >
      {items === null ? (
        <div className="flex items-center justify-center gap-2 py-16 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" aria-hidden />
          読み込み中…
        </div>
      ) : items.length === 0 ? (
        <EmptyState
          icon={Sparkles}
          title="スキルはまだありません"
          description="業務知識をまとめたスキルを作るか、組織のスキルストアから追加しましょう。"
          action={
            <div className="flex flex-wrap justify-center gap-2">
              <Button onClick={() => router.push("/skills/new")}>
                <Sparkles className="size-4" aria-hidden />
                スキルを作成
              </Button>
              <Button variant="outline" onClick={scrollToStore}>
                <Store className="size-4" aria-hidden />
                ストアから追加
              </Button>
            </div>
          }
        />
      ) : (
        <ul className="grid gap-3 sm:grid-cols-2">
          {items.map((meta) => {
            const busy = pending === meta.id;
            return (
              <ArtifactCard
                key={meta.id}
                name={meta.name}
                meta={`v${meta.currentVersion}・更新 ${new Date(meta.updatedAt).toLocaleDateString("ja-JP")}`}
                pending={busy}
                primary={
                  <Button size="sm" onClick={() => void startChat(meta)} disabled={busy}>
                    <MessageSquareText className="size-4" aria-hidden />
                    このスキルでチャット
                  </Button>
                }
                menu={[
                  {
                    key: "edit",
                    label: "編集",
                    icon: Pencil,
                    onSelect: () => router.push(`/skills/${meta.id}`),
                    disabled: busy,
                  },
                  { key: "share", label: "共有", icon: Share2, onSelect: () => setDialog({ kind: "share", meta }) },
                  {
                    key: "publish",
                    label: "ストアへ公開",
                    icon: UploadCloud,
                    onSelect: () => void publish(meta),
                    disabled: busy,
                  },
                  {
                    key: "versions",
                    label: "バージョン履歴",
                    icon: History,
                    onSelect: () => setDialog({ kind: "versions", meta }),
                  },
                  {
                    key: "delete",
                    label: "削除",
                    icon: Trash2,
                    destructive: true,
                    separatorBefore: true,
                    onSelect: () => setDialog({ kind: "delete", meta }),
                  },
                ]}
              />
            );
          })}
        </ul>
      )}

      <SkillStoreSection />

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
      <ConfirmDialog
        open={dialog.kind === "delete"}
        onOpenChange={(open) => !open && setDialog({ kind: "closed" })}
        title="スキルを削除しますか？"
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
