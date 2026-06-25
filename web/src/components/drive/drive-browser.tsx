"use client";

import * as React from "react";
import { useRouter, useSearchParams } from "next/navigation";
import { ArrowDownUp, FolderPlus, Loader2, Upload, UploadCloud } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { EmptyState } from "@/components/ui/empty-state";
import { toast } from "@/components/ui/use-toast";
import { useInfiniteList, useInfiniteSentinel } from "@/hooks/use-infinite-list";
import {
  breadcrumb,
  createFolder,
  deleteFile,
  deleteFolder,
  listChildren,
  triggerDownload,
  updateFile,
  updateFolder,
  uploadFile,
  type CrumbResponse,
  type NodeResponse,
  type SortField,
} from "@/lib/storage";
import { cn } from "@/lib/utils";

import { ConfirmDialog, MoveDialog, TextPromptDialog } from "./dialogs";
import { NodeRow, type NodeAction } from "./node-row";
import { Breadcrumbs, LoadingRow } from "./primitives";
import { ShareDialog } from "./share-dialog";
import { VersionsDialog } from "./versions-dialog";

type SortOption = { key: string; label: string; sort: SortField; desc: boolean };
const SORT_OPTIONS: SortOption[] = [
  { key: "name-asc", label: "名前（A→Z）", sort: "name", desc: false },
  { key: "name-desc", label: "名前（Z→A）", sort: "name", desc: true },
  { key: "updated-desc", label: "更新が新しい順", sort: "updated", desc: true },
  { key: "updated-asc", label: "更新が古い順", sort: "updated", desc: false },
  { key: "size-desc", label: "サイズが大きい順", sort: "size", desc: true },
  { key: "size-asc", label: "サイズが小さい順", sort: "size", desc: false },
];

type DialogKind = "newfolder" | "rename" | "move" | "share" | "versions" | "delete" | null;

type UploadState = { name: string; fraction: number };

/// Drive 本体。フォルダブラウズ（無限スクロール・パンくず・ソート）＋ D&D アップロード
/// ＋ 移動/リネーム/削除/共有/版履歴を提供する。現在フォルダは `?folder=<id>` で表す。
export function DriveBrowser() {
  const router = useRouter();
  const searchParams = useSearchParams();
  const folderId = searchParams.get("folder");

  const [sortKey, setSortKey] = React.useState<string>("name-asc");
  const sortOption = SORT_OPTIONS.find((o) => o.key === sortKey) ?? SORT_OPTIONS[0];

  const [crumbs, setCrumbs] = React.useState<CrumbResponse[]>([]);
  const [dialog, setDialog] = React.useState<DialogKind>(null);
  const [activeNode, setActiveNode] = React.useState<NodeResponse | null>(null);
  const [uploads, setUploads] = React.useState<UploadState[]>([]);
  const [dragging, setDragging] = React.useState(false);
  const fileInputRef = React.useRef<HTMLInputElement>(null);
  // 「新しいバージョン」用の隠し入力と対象ノード。
  const versionInputRef = React.useRef<HTMLInputElement>(null);
  const versionTargetRef = React.useRef<NodeResponse | null>(null);

  const fetchPage = React.useCallback(
    (cursor?: string) =>
      listChildren({
        parentId: folderId ?? undefined,
        sort: sortOption.sort,
        desc: sortOption.desc,
        cursor,
        limit: 50,
      }),
    [folderId, sortOption.sort, sortOption.desc],
  );
  const list = useInfiniteList<NodeResponse>(fetchPage, [folderId, sortOption.sort, sortOption.desc]);
  const sentinelRef = useInfiniteSentinel(list.loadMore, list.hasMore && !list.loading);

  // パンくず（現在フォルダが変わるたび取得）。ルートは空。
  React.useEffect(() => {
    if (!folderId) {
      setCrumbs([]);
      return;
    }
    let active = true;
    breadcrumb(folderId)
      .then((c) => {
        if (active) setCrumbs(c);
      })
      .catch(() => {
        if (active) setCrumbs([]);
      });
    return () => {
      active = false;
    };
  }, [folderId]);

  const navigateTo = (id: string | null) => {
    router.push(id ? `/drive?folder=${id}` : "/drive", { scroll: false });
  };

  // --- アップロード（D&D / ボタン） ---
  const runUploads = async (files: File[]) => {
    if (files.length === 0) return;
    for (const file of files) {
      setUploads((prev) => [...prev, { name: file.name, fraction: 0 }]);
      try {
        await uploadFile({
          file,
          parentId: folderId ?? undefined,
          onProgress: (fraction) =>
            setUploads((prev) =>
              prev.map((u) => (u.name === file.name ? { ...u, fraction } : u)),
            ),
        });
      } catch (e) {
        toast({
          variant: "destructive",
          title: `「${file.name}」のアップロードに失敗`,
          description: e instanceof Error ? e.message : String(e),
        });
      } finally {
        setUploads((prev) => prev.filter((u) => u.name !== file.name));
      }
    }
    toast({ title: "アップロードが完了しました" });
    list.reload();
  };

  const onDrop = (e: React.DragEvent) => {
    e.preventDefault();
    setDragging(false);
    const files = Array.from(e.dataTransfer.files);
    void runUploads(files);
  };

  // 既存ファイルへ新しいバージョンをアップロードする（target_node_id 指定）。
  const uploadNewVersion = async (file: File, node: NodeResponse) => {
    setUploads((prev) => [...prev, { name: file.name, fraction: 0 }]);
    try {
      await uploadFile({
        file,
        targetNodeId: node.id,
        onProgress: (fraction) =>
          setUploads((prev) => prev.map((u) => (u.name === file.name ? { ...u, fraction } : u))),
      });
      toast({ title: "新しいバージョンをアップロードしました", description: node.name });
      list.reload();
    } catch (e) {
      toast({
        variant: "destructive",
        title: "バージョンの追加に失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setUploads((prev) => prev.filter((u) => u.name !== file.name));
    }
  };

  // --- 行アクション ---
  const handleAction = (action: NodeAction, node: NodeResponse) => {
    switch (action) {
      case "open":
        navigateTo(node.id);
        break;
      case "download":
        triggerDownload(node.id).catch((e) =>
          toast({
            variant: "destructive",
            title: "ダウンロードに失敗しました",
            description: e instanceof Error ? e.message : String(e),
          }),
        );
        break;
      case "newversion":
        versionTargetRef.current = node;
        versionInputRef.current?.click();
        break;
      default:
        setActiveNode(node);
        setDialog(action);
    }
  };

  const closeDialog = () => setDialog(null);

  return (
    <div className="flex flex-col gap-4">
      {/* ツールバー */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <Breadcrumbs crumbs={crumbs} onNavigate={navigateTo} />
        <div className="flex items-center gap-2">
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="outline" size="sm">
                <ArrowDownUp className="size-4" aria-hidden />
                {sortOption.label}
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              {SORT_OPTIONS.map((o) => (
                <DropdownMenuItem key={o.key} onSelect={() => setSortKey(o.key)}>
                  {o.label}
                </DropdownMenuItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              setActiveNode(null);
              setDialog("newfolder");
            }}
          >
            <FolderPlus className="size-4" aria-hidden />
            新規フォルダ
          </Button>
          <Button size="sm" onClick={() => fileInputRef.current?.click()}>
            <Upload className="size-4" aria-hidden />
            アップロード
          </Button>
          <input
            ref={fileInputRef}
            type="file"
            multiple
            hidden
            onChange={(e) => {
              void runUploads(Array.from(e.target.files ?? []));
              e.target.value = "";
            }}
          />
          <input
            ref={versionInputRef}
            type="file"
            hidden
            onChange={(e) => {
              const file = e.target.files?.[0];
              const node = versionTargetRef.current;
              if (file && node) void uploadNewVersion(file, node);
              versionTargetRef.current = null;
              e.target.value = "";
            }}
          />
        </div>
      </div>

      {/* アップロード進捗 */}
      {uploads.length > 0 ? (
        <div className="flex flex-col gap-2 rounded-lg border border-border bg-card p-3">
          {uploads.map((u) => (
            <div key={u.name} className="flex items-center gap-3">
              <Loader2 className="size-4 shrink-0 animate-spin text-primary" aria-hidden />
              <span className="min-w-0 flex-1 truncate text-sm">{u.name}</span>
              <div className="h-1.5 w-28 overflow-hidden rounded-full bg-secondary">
                <div
                  className="h-full rounded-full bg-primary transition-[width] duration-150"
                  style={{ width: `${Math.round(u.fraction * 100)}%` }}
                />
              </div>
            </div>
          ))}
        </div>
      ) : null}

      {/* ドロップ領域＋一覧 */}
      <div
        onDragOver={(e) => {
          e.preventDefault();
          if (!dragging) setDragging(true);
        }}
        onDragLeave={(e) => {
          if (e.currentTarget === e.target) setDragging(false);
        }}
        onDrop={onDrop}
        className={cn(
          "relative min-h-[16rem] rounded-xl border border-border bg-card p-2 transition-colors",
          dragging && "border-primary/60 ring-2 ring-primary/30",
        )}
      >
        {dragging ? (
          <div className="pointer-events-none absolute inset-0 z-10 flex flex-col items-center justify-center gap-2 rounded-xl bg-primary/5 text-primary">
            <UploadCloud className="size-8" aria-hidden />
            <p className="text-sm font-medium">ここにドロップしてアップロード</p>
          </div>
        ) : null}

        {list.loading ? (
          <LoadingRow />
        ) : list.error ? (
          <p className="px-3 py-10 text-center text-sm text-destructive">{list.error}</p>
        ) : list.items.length === 0 ? (
          <EmptyState
            icon={UploadCloud}
            title="このフォルダは空です"
            description="ファイルをドラッグ＆ドロップするか、アップロードボタンから追加できます。"
          />
        ) : (
          <div className="flex flex-col">
            {list.items.map((node) => (
              <NodeRow key={node.id} node={node} onAction={handleAction} />
            ))}
            {list.hasMore ? <div ref={sentinelRef}>{list.loadingMore ? <LoadingRow /> : null}</div> : null}
          </div>
        )}
      </div>

      {/* ダイアログ群 */}
      <TextPromptDialog
        open={dialog === "newfolder"}
        onOpenChange={(o) => (o ? null : closeDialog())}
        title="新規フォルダ"
        label="フォルダ名"
        submitLabel="作成"
        onSubmit={async (name) => {
          await createFolder(name, folderId ?? undefined);
          list.reload();
        }}
      />

      <TextPromptDialog
        open={dialog === "rename"}
        onOpenChange={(o) => (o ? null : closeDialog())}
        title="名前を変更"
        label="新しい名前"
        initialValue={activeNode?.name ?? ""}
        submitLabel="変更"
        onSubmit={async (name) => {
          if (!activeNode) return;
          if (activeNode.kind === "folder") await updateFolder(activeNode.id, { name });
          else await updateFile(activeNode.id, { name });
          list.reload();
        }}
      />

      <MoveDialog
        open={dialog === "move"}
        onOpenChange={(o) => (o ? null : closeDialog())}
        node={activeNode}
        onMove={async (dest) => {
          if (!activeNode) return;
          if (activeNode.kind === "folder") await updateFolder(activeNode.id, { move: dest });
          else await updateFile(activeNode.id, { move: dest });
          list.reload();
        }}
      />

      <ShareDialog
        open={dialog === "share"}
        onOpenChange={(o) => (o ? null : closeDialog())}
        node={activeNode}
      />

      <VersionsDialog
        open={dialog === "versions"}
        onOpenChange={(o) => (o ? null : closeDialog())}
        node={activeNode}
        onRestored={() => list.reload()}
      />

      <ConfirmDialog
        open={dialog === "delete"}
        onOpenChange={(o) => (o ? null : closeDialog())}
        title={activeNode ? `「${activeNode.name}」をゴミ箱へ移動` : "削除"}
        description="ゴミ箱からはいつでも復元できます。"
        confirmLabel="ゴミ箱へ移動"
        destructive
        onConfirm={async () => {
          if (!activeNode) return;
          if (activeNode.kind === "folder") await deleteFolder(activeNode.id);
          else await deleteFile(activeNode.id);
          list.reload();
        }}
      />
    </div>
  );
}
