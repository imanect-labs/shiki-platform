"use client";

import * as React from "react";
import { useRouter, useSearchParams } from "next/navigation";
import {
  ArrowDownUp,
  Check,
  FileSpreadsheet,
  FileText,
  FolderPlus,
  LayoutGrid,
  List as ListIcon,
  Loader2,
  Plus,
  Presentation,
  Search,
  Upload,
  UploadCloud,
  X,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { EmptyState } from "@/components/ui/empty-state";
import { toast } from "@/components/ui/use-toast";
import { useInfiniteList, useInfiniteSentinel } from "@/hooks/use-infinite-list";
import { useContentSearch, type ContentHit } from "@/lib/drive-search";
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
import { NodeCard } from "./node-card";
import { NodeRow, type NodeAction } from "./node-row";
import { Breadcrumbs, ListHeader, LoadingRow } from "./primitives";
import { ShareDialog } from "./share-dialog";
import { VersionsDialog } from "./versions-dialog";

/// 列見出しをクリックした時の既定の並び順（名前は昇順、更新日時・サイズは降順）。
const DEFAULT_DESC: Record<SortField, boolean> = { name: false, updated: true, size: true };

/// 並べ替えメニューの選択肢（ラベルは列見出しと統一）。
const SORT_OPTIONS: { field: SortField; label: string }[] = [
  { field: "name", label: "名前" },
  { field: "updated", label: "更新日時" },
  { field: "size", label: "サイズ" },
];

type ViewMode = "list" | "grid";

type DialogKind = "newfolder" | "rename" | "move" | "share" | "versions" | "delete" | null;

type UploadState = { name: string; fraction: number };

/// Drive 本体。フォルダブラウズ（無限スクロール・パンくず・ソート）＋ D&D アップロード
/// ＋ 移動/リネーム/削除/共有/版履歴を提供する。現在フォルダは `?folder=<id>` で表す。
export function DriveBrowser() {
  const router = useRouter();
  const searchParams = useSearchParams();
  const folderId = searchParams.get("folder");

  const [sort, setSort] = React.useState<SortField>("name");
  const [desc, setDesc] = React.useState(false);
  // 同じ列なら昇降トグル、別の列なら既定方向で並べ替える（OneDrive 風）。
  const onSort = (field: SortField) => {
    if (field === sort) setDesc((d) => !d);
    else {
      setSort(field);
      setDesc(DEFAULT_DESC[field]);
    }
  };
  const activeSortLabel = SORT_OPTIONS.find((o) => o.field === sort)?.label ?? "名前";

  // 表示モード（一覧/グリッド）。好みは localStorage に保存して次回も維持する。
  const [view, setView] = React.useState<ViewMode>("list");
  React.useEffect(() => {
    const saved = window.localStorage.getItem("drive:view");
    if (saved === "grid" || saved === "list") setView(saved);
  }, []);
  const changeView = (v: ViewMode) => {
    setView(v);
    try {
      window.localStorage.setItem("drive:view", v);
    } catch {
      /* 永続化失敗は無視（プライベートモード等） */
    }
  };

  // 新規ドキュメント作成（ドキュメント/スライド/スプレッドシート）はまだダミー。
  // バックエンド（生成・テンプレート）実装までは「準備中」を知らせる。
  const createDocument = (label: string) =>
    toast({ title: `${label}を作成`, description: "この機能は近日対応予定です。" });

  // 検索: 入力は即時、クエリは少し待ってから反映（打鍵ごとの再取得を抑える）。
  // ⌘K パレットの「"q"で検索」からは ?q= で遷移してくるため、URL からも初期化・追従する。
  const urlQuery = searchParams.get("q") ?? "";
  const [searchInput, setSearchInput] = React.useState(urlQuery);
  const [query, setQuery] = React.useState(urlQuery.trim());
  React.useEffect(() => {
    if (urlQuery) setSearchInput(urlQuery);
  }, [urlQuery]);
  React.useEffect(() => {
    const t = setTimeout(() => setQuery(searchInput.trim()), 300);
    return () => clearTimeout(t);
  }, [searchInput]);
  const searching = query.length > 0;
  // 内容一致（permission-aware RAG）。名前一致と統合してスコア順で出す。
  const content = useContentSearch(query, searching);

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
        // 検索中はフォルダを跨いで横断検索（parent を渡さない）。
        parentId: searching ? undefined : (folderId ?? undefined),
        sort,
        desc,
        cursor,
        limit: 50,
        q: searching ? query : undefined,
      }),
    [folderId, sort, desc, searching, query],
  );
  const list = useInfiniteList<NodeResponse>(fetchPage, [folderId, sort, desc, searching, query]);
  const sentinelRef = useInfiniteSentinel(list.loadMore, list.hasMore && !list.loading);
  // 表示する内容一致行（名前一致にも出るファイルは名前行に譲る）。件数表示と共有する。
  const contentRows = searching
    ? content.hits.filter((h) => !list.items.some((n) => n.id === h.fileId))
    : [];

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
    let succeeded = 0;
    let failed = 0;
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
        succeeded += 1;
      } catch (e) {
        failed += 1;
        toast({
          variant: "destructive",
          title: `「${file.name}」のアップロードに失敗`,
          description: e instanceof Error ? e.message : String(e),
        });
      } finally {
        setUploads((prev) => prev.filter((u) => u.name !== file.name));
      }
    }
    // 成功が 1 件もなければ「完了」トーストは出さない（失敗トーストと矛盾させない）。
    if (succeeded > 0) {
      toast({
        title:
          failed === 0
            ? "アップロードが完了しました"
            : `${succeeded} 件をアップロードしました（${failed} 件失敗）`,
      });
      list.reload();
    }
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
        // フォルダは配下をブラウズ。ファイルはダウンロードで内容を取得する
        // （インラインのファイルプレビューは現状未提供）。
        if (node.kind === "folder") navigateTo(node.id);
        else
          triggerDownload(node.id).catch((e) =>
            toast({
              variant: "destructive",
              title: "ダウンロードに失敗しました",
              description: e instanceof Error ? e.message : String(e),
            }),
          );
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
      {/* コマンドバー: 左=新規作成＋操作＋並べ替え＋表示切替 / 右=検索。高さ(h-9)・角丸(lg)を揃える。 */}
      <div className="flex flex-wrap items-center gap-2.5">
        {/* 新規作成（丸＋）。ドキュメント/スライド/スプレッドシート（現状ダミー）。 */}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button size="icon" className="size-9 shrink-0 rounded-full" aria-label="新規作成">
              <Plus className="size-5" aria-hidden />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="w-52">
            <DropdownMenuLabel>新規作成</DropdownMenuLabel>
            <DropdownMenuItem onSelect={() => createDocument("ドキュメント")}>
              <FileText className="text-blue-600" aria-hidden />
              ドキュメント
            </DropdownMenuItem>
            <DropdownMenuItem onSelect={() => createDocument("スライド")}>
              <Presentation className="text-orange-500" aria-hidden />
              スライド
            </DropdownMenuItem>
            <DropdownMenuItem onSelect={() => createDocument("スプレッドシート")}>
              <FileSpreadsheet className="text-green-600" aria-hidden />
              スプレッドシート
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>

        <Button
          variant="outline"
          className="rounded-lg"
          onClick={() => {
            setActiveNode(null);
            setDialog("newfolder");
          }}
        >
          <FolderPlus className="size-4" aria-hidden />
          新規フォルダ
        </Button>
        <Button className="rounded-lg" onClick={() => fileInputRef.current?.click()}>
          <Upload className="size-4" aria-hidden />
          アップロード
        </Button>

        <div className="mx-0.5 hidden h-6 w-px bg-border/70 sm:block" aria-hidden />

        {/* 並べ替え */}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="ghost"
              className="rounded-lg text-muted-foreground hover:text-foreground"
              aria-label="並べ替え"
            >
              <ArrowDownUp className="size-4" aria-hidden />
              <span className="hidden sm:inline">{activeSortLabel}</span>
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="w-44">
            <DropdownMenuLabel>並べ替え</DropdownMenuLabel>
            {SORT_OPTIONS.map((o) => (
              <DropdownMenuItem key={o.field} onSelect={() => setSort(o.field)}>
                <Check
                  className={cn("size-4", sort === o.field ? "opacity-100" : "opacity-0")}
                  aria-hidden
                />
                {o.label}
              </DropdownMenuItem>
            ))}
            <DropdownMenuSeparator />
            <DropdownMenuItem onSelect={() => setDesc(false)}>
              <Check className={cn("size-4", !desc ? "opacity-100" : "opacity-0")} aria-hidden />
              昇順
            </DropdownMenuItem>
            <DropdownMenuItem onSelect={() => setDesc(true)}>
              <Check className={cn("size-4", desc ? "opacity-100" : "opacity-0")} aria-hidden />
              降順
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>

        {/* 表示切替（一覧/グリッド） */}
        <div className="flex items-center rounded-lg border border-border p-0.5">
          <button
            type="button"
            onClick={() => changeView("list")}
            aria-label="一覧表示"
            aria-pressed={view === "list"}
            className={cn(
              "flex size-7 items-center justify-center rounded-md transition-colors",
              view === "list"
                ? "bg-accent text-foreground"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            <ListIcon className="size-4" aria-hidden />
          </button>
          <button
            type="button"
            onClick={() => changeView("grid")}
            aria-label="グリッド表示"
            aria-pressed={view === "grid"}
            className={cn(
              "flex size-7 items-center justify-center rounded-md transition-colors",
              view === "grid"
                ? "bg-accent text-foreground"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            <LayoutGrid className="size-4" aria-hidden />
          </button>
        </div>

        {/* 検索（右寄せ） */}
        <div className="relative ml-auto w-full sm:w-60 md:w-72">
          <Search
            className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground/70"
            aria-hidden
          />
          <Input
            type="search"
            value={searchInput}
            onChange={(e) => setSearchInput(e.target.value)}
            placeholder="ドライブを検索"
            aria-label="ドライブを検索"
            className="h-9 rounded-lg pl-9 pr-9"
          />
          {searchInput ? (
            <button
              type="button"
              onClick={() => setSearchInput("")}
              aria-label="検索をクリア"
              className="absolute right-2 top-1/2 grid size-6 -translate-y-1/2 place-items-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            >
              <X className="size-4" aria-hidden />
            </button>
          ) : null}
        </div>

        {/* 隠しファイル入力（アップロード／新バージョン） */}
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

      {/* 現在地（パンくず/検索結果）と件数。パンくずはサブフォルダにいる時だけ
          （ルートはシェルの見出し「ドライブ」と重複するため出さない）。 */}
      {searching || crumbs.length > 0 || (!list.loading && list.items.length > 0) ? (
        <div className="flex min-h-7 items-center justify-between gap-3 px-1">
          <div className="min-w-0">
            {searching ? (
              <p className="truncate text-sm text-muted-foreground">
                「{query}」の検索結果
                <a
                  href={`/search?q=${encodeURIComponent(query)}`}
                  className="ml-2 text-xs text-primary underline-offset-2 hover:underline"
                >
                  詳細検索（引用・絞込の内訳）
                </a>
              </p>
            ) : crumbs.length > 0 ? (
              <Breadcrumbs crumbs={crumbs} onNavigate={navigateTo} />
            ) : null}
          </div>
          {searching ? (
            !list.loading && !content.loading && contentRows.length + list.items.length > 0 ? (
              <span className="shrink-0 text-[13px] tabular-nums text-muted-foreground">
                {[
                  contentRows.length > 0 ? `内容一致 ${contentRows.length} 件` : null,
                  list.items.length > 0
                    ? `名前一致 ${list.items.length}${list.hasMore ? "+" : ""} 件`
                    : null,
                ]
                  .filter(Boolean)
                  .join("・")}
              </span>
            ) : null
          ) : !list.loading && list.items.length > 0 ? (
            <span className="shrink-0 text-[13px] tabular-nums text-muted-foreground">
              {list.items.length}
              {list.hasMore ? "+" : ""} 件
            </span>
          ) : null}
        </div>
      ) : null}

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

      {/* ドロップ領域＋一覧（カード枠なし・背景に直接） */}
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
          "rule-soft relative min-h-[16rem] rounded-xl transition-colors",
          dragging && "bg-primary/5 ring-2 ring-primary/30",
        )}
      >
        {dragging ? (
          <div className="pointer-events-none absolute inset-0 z-10 flex flex-col items-center justify-center gap-2 rounded-xl bg-primary/5 text-primary">
            <UploadCloud className="size-8" aria-hidden />
            <p className="text-sm font-medium">ここにドロップしてアップロード</p>
          </div>
        ) : null}

        {view === "list" ? <ListHeader sort={sort} desc={desc} onSort={onSort} /> : null}

        {searching ? (
          <ContentHitRows hits={contentRows} onOpen={(h) => navigateTo(h.folderId)} />
        ) : null}

        {list.loading ? (
          <LoadingRow />
        ) : list.error ? (
          <p className="px-3 py-10 text-center text-sm text-destructive">{list.error}</p>
        ) : list.items.length === 0 ? (
          searching ? (
            content.loading ? (
              <LoadingRow />
            ) : content.hits.length === 0 ? (
              <EmptyState
                icon={Search}
                title="見つかりませんでした"
                description={`「${query}」に名前・内容が一致するファイル・フォルダはありません。`}
              />
            ) : null
          ) : (
            <EmptyState
              icon={UploadCloud}
              title="このフォルダは空です"
              description="ファイルをドラッグ＆ドロップするか、アップロードボタンから追加できます。"
            />
          )
        ) : view === "grid" ? (
          <div className="grid grid-cols-2 gap-3 p-1 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5">
            {list.items.map((node) => (
              <NodeCard key={node.id} node={node} onAction={handleAction} />
            ))}
            {list.hasMore ? (
              <div ref={sentinelRef} className="col-span-full">
                {list.loadingMore ? <LoadingRow /> : null}
              </div>
            ) : null}
          </div>
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

/// 内容一致（RAG）のヒット行。名前一致（NodeRow）の上に関連度順で並べ、
/// スニペットで「なぜヒットしたか」を見せる。スコアは並び順にのみ使い表示しない
/// （エンドユーザーに内部指標を見せない）。選択でファイルのあるフォルダへ移動。
function ContentHitRows({
  hits,
  onOpen,
}: {
  hits: ContentHit[];
  onOpen: (hit: ContentHit) => void;
}) {
  if (hits.length === 0) return null;
  return (
    <div className="flex flex-col" aria-label="内容が一致した文書">
      {hits.map((h) => (
        <button
          key={h.fileId}
          type="button"
          onClick={() => onOpen(h)}
          className="shiki-dash-bottom flex w-full items-start gap-3 px-3 py-2.5 text-left outline-none transition-colors hover:bg-accent focus-visible:bg-accent"
        >
          <FileText className="mt-0.5 size-[18px] shrink-0 text-primary" aria-hidden />
          <span className="min-w-0 flex-1">
            <span className="block truncate text-sm font-medium text-foreground">{h.fileName}</span>
            <span className="mt-0.5 line-clamp-1 text-xs text-muted-foreground">{h.snippet}</span>
          </span>
        </button>
      ))}
    </div>
  );
}
