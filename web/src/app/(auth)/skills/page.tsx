"use client";

/// スキル管理ページ（Task 6.11）: 一覧・作成・編集（新版）・共有・バージョン履歴。

import * as React from "react";
import { useRouter } from "next/navigation";
import { History, Loader2, MessageSquareText, Pencil, Share2, Sparkles, Trash2, UploadCloud } from "lucide-react";

import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { toast } from "@/components/ui/use-toast";
import { ArtifactShareDialog } from "@/components/artifacts/share-dialog";
import { SkillEditorDialog } from "@/components/artifacts/skill-editor";
import { VersionsDialog } from "@/components/artifacts/versions-dialog";
import {
  deleteArtifact,
  getSkill,
  listArtifacts,
  type ArtifactMeta,
  type SkillVersion,
} from "@/lib/artifact-api";
import { createThread } from "@/lib/chat-api";
import { publishSkill } from "@/lib/skill-registry-api";
import { SkillStoreSection } from "@/components/artifacts/skill-store-section";

type DialogState =
  | { kind: "closed" }
  | { kind: "create" }
  | { kind: "edit"; meta: ArtifactMeta; skill: SkillVersion }
  | { kind: "share"; meta: ArtifactMeta }
  | { kind: "versions"; meta: ArtifactMeta };

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

  const openEdit = async (meta: ArtifactMeta) => {
    setPending(meta.id);
    try {
      const skill = await getSkill(meta.id);
      setDialog({ kind: "edit", meta, skill });
    } catch (e) {
      toast({
        variant: "destructive",
        title: "読み込みに失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setPending(null);
    }
  };

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

  const remove = async (meta: ArtifactMeta) => {
    if (!window.confirm(`スキル「${meta.name}」を削除しますか？（バージョン履歴は保持されます）`)) return;
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
          <h1 className="text-lg font-semibold">スキル</h1>
          <p className="text-sm text-muted-foreground">
            指示文・知識スコープ・許可ツールをまとめて、チャットに適用できます。
          </p>
        </div>
        <Button onClick={() => setDialog({ kind: "create" })}>
          <Sparkles className="size-4" aria-hidden />
          スキルを作成
        </Button>
      </div>

      {items === null ? (
        <div className="flex items-center justify-center gap-2 py-16 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" aria-hidden />
          読み込み中…
        </div>
      ) : items.length === 0 ? (
        <EmptyState
          icon={Sparkles}
          title="スキルはまだありません"
          description="「スキルを作成」から、業務知識をチャットに教え込みましょう。"
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
                <Button size="sm" onClick={() => void startChat(meta)} disabled={pending === meta.id}>
                  <MessageSquareText className="size-4" aria-hidden />
                  このスキルでチャット
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  aria-label={`${meta.name} を編集`}
                  onClick={() => void openEdit(meta)}
                  disabled={pending === meta.id}
                >
                  <Pencil className="size-4" aria-hidden />
                  編集
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
                  aria-label={`${meta.name} をレジストリへ公開`}
                  title="レジストリへ公開（スキルストアからインストール可能になる）"
                  onClick={() => void publish(meta)}
                  disabled={pending === meta.id}
                >
                  <UploadCloud className="size-4" aria-hidden />
                  公開
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

      <SkillStoreSection />

      <SkillEditorDialog
        open={dialog.kind === "create" || dialog.kind === "edit"}
        onOpenChange={(open) => !open && setDialog({ kind: "closed" })}
        editing={dialog.kind === "edit" ? dialog.skill : null}
        editingName={dialog.kind === "edit" ? dialog.meta.name : ""}
        onSaved={() => {
          reload();
          toast({ title: "保存しました" });
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
