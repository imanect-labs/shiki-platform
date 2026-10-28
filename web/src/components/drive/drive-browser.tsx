"use client";

import * as React from "react";
import { flushSync } from "react-dom";
import { useRouter, useSearchParams } from "next/navigation";
import { FileText, FolderPlus, Loader2, Search, Upload, UploadCloud, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { EmptyState } from "@/components/ui/empty-state";
import { toast } from "@/components/ui/use-toast";
import { MobilePageTitle } from "@/components/shell/page-header-context";
import { useInfiniteList, useInfiniteSentinel } from "@/hooks/use-infinite-list";
import { useCreateContent } from "@/hooks/use-create-content";
import { useMe } from "@/hooks/use-me";
import { createOfficeSession, OfficeSessionError } from "@/lib/office-api";
import { useContentSearch, type ContentHit } from "@/lib/drive-search";
import { extensionChangeError, stemRange } from "@/lib/file-name";
import { isOfficeFile, resourcePath } from "@/lib/resource-link";
import {
  breadcrumb,
  createFolder,
  deleteFile,
  deleteFolder,
  isConflict,
  listChildren,
  triggerDownload,
  updateFile,
  updateFolder,
  type CrumbResponse,
  type NodeResponse,
  type SortField,
} from "@/lib/storage";
import { cn } from "@/lib/utils";

import { ConfirmDialog, MoveDialog, TextPromptDialog } from "./dialogs";
import { DriveChrome } from "./drive-chrome";
import { NewFab, NewMenuDesktop, type CreateHandlers } from "./new-menu";
import { NodeCard } from "./node-card";
import { NodeRow, type NodeAction } from "./node-row";
import { Breadcrumbs, ListHeader, LoadingRow, MobileBreadcrumbs } from "./primitives";
import { ShareDialog } from "./share-dialog";
import { SortMenu, ViewToggle, type ViewMode } from "./toolbar";
import { UploadConflictDialog } from "./upload-conflict-dialog";
import { useDriveUploads } from "./use-drive-uploads";
import { VersionsDialog } from "./versions-dialog";

/// 列見出しをクリックした時の既定の並び順（名前は昇順、更新日時・サイズは降順）。
const DEFAULT_DESC: Record<SortField, boolean> = { name: false, updated: true, size: true };

type DialogKind = "newfolder" | "rename" | "move" | "share" | "versions" | "delete" | null;

const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e));

/// Drive 本体。フォルダブラウズ（無限スクロール・パンくず・ソート）＋ D&D アップロード
/// ＋ 移動/リネーム/削除/共有/版履歴を提供する。現在フォルダは `?folder=<id>` で表す。
///
/// スマホ（md 未満）はタイトルバーが無いので、本文の先頭に見出し（パンくず＋フォルダ名）を
/// 置き、その右に並べ替えと表示切替を 1 段で並べる。新規作成・アップロードは右下の
/// 「＋ 新規」に集め、検索は右上の浮いたボタン（DriveChrome）、区画の切替は上端のタブ
/// （DriveTabs・drive/layout.tsx）から出す。
export function DriveBrowser() {
  const router = useRouter();
  const searchParams = useSearchParams();
  const folderId = searchParams.get("folder");
  // 更新者列の「自分」判定用（行ごとに /me を叩かず親で 1 回だけ解決する）。
  const me = useMe();

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

  // 表示モード（一覧/グリッド）。好みは localStorage に保存して次回も維持する。
  const [view, setView] = React.useState<ViewMode>("list");
  React.useEffect(() => {
    try {
      const saved = window.localStorage.getItem("drive:view");
      if (saved === "grid" || saved === "list") setView(saved);
    } catch {
      /* 読めない環境（プライベートモード等）は既定のまま */
    }
  }, []);
  const changeView = (v: ViewMode) => {
    setView(v);
    try {
      window.localStorage.setItem("drive:view", v);
    } catch {
      /* 永続化失敗は無視（プライベートモード等） */
    }
  };

  // 新規作成（ノート/ドキュメント/スライド/CSV）→ エディタ遷移。チャット「+」の
  // 「作成」サブメニューと共用の集約フック（#333）。保存先は現在フォルダ。
  const content = useCreateContent({ parentId: folderId });
  const create: CreateHandlers = {
    note: () => void content.createNoteAndOpen(),
    document: () => void content.createDocumentAndOpen(),
    sheet: () => void content.createSheetAndOpen(),
    slide: () => void content.createSlideAndOpen(),
    csv: () => void content.createCsvAndOpen(),
  };

  const download = (node: NodeResponse) =>
    triggerDownload(node.id).catch((e) =>
      toast({ variant: "destructive", title: "ダウンロードに失敗しました", description: errorText(e) }),
    );

  // Office 文書を開く。編集セッションが発行できればエディタへ、Office 未配備（503）・
  // 未対応（404）はファイル画面（プレビューできません＋ダウンロード）へフォールバックする
  // （開けないのに遷移して空画面を見せない・黙ってダウンロードもしない）。
  const openOffice = async (node: NodeResponse) => {
    try {
      await createOfficeSession(node.id);
      router.push(`/office/${node.id}`);
    } catch (e) {
      if (e instanceof OfficeSessionError && e.kind === "unavailable") {
        toast({
          title: "Office 編集サービスが未起動です",
          description: "ダウンロードして開けます。管理者に office profile の有効化を確認してください。",
        });
      }
      router.push(`/file/${node.id}`);
    }
  };

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
  const contentSearch = useContentSearch(query, searching);

  // スマホの検索欄は右上の検索アイコンで開く（入力中・?q= 付きで来た時は開いたまま）。
  const [mobileSearchOpen, setMobileSearchOpen] = React.useState(Boolean(urlQuery));
  const searchInputRef = React.useRef<HTMLInputElement>(null);
  const searchVisible = mobileSearchOpen || searchInput.length > 0;
  const toggleMobileSearch = React.useCallback(() => {
    if (searchVisible) {
      setMobileSearchOpen(false);
      setSearchInput("");
      return;
    }
    // 欄を出してから同じタップの中でフォーカスする（iOS はユーザー操作の外で focus しても
    // キーボードを出さないため、effect に回さず flushSync で描画を先に確定させる）。
    flushSync(() => setMobileSearchOpen(true));
    searchInputRef.current?.focus();
  }, [searchVisible]);

  const [crumbs, setCrumbs] = React.useState<CrumbResponse[]>([]);
  const [dialog, setDialog] = React.useState<DialogKind>(null);
  const [activeNode, setActiveNode] = React.useState<NodeResponse | null>(null);
  const [dragging, setDragging] = React.useState(false);
  const fileInputRef = React.useRef<HTMLInputElement>(null);
  /// ファイルを選んだら呼ぶ後始末（スマホの「＋ 新規」のシートを閉じる）。開閉 state は NewFab が持つ
  /// （ここに持つと開閉のたびに一覧ごと描き直す）ので、閉じる関数だけを受け取っておく。
  const afterPickRef = React.useRef<(() => void) | null>(null);
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
    ? contentSearch.hits.filter((h) => !list.items.some((n) => n.id === h.fileId))
    : [];

  const uploader = useDriveUploads({ folderId, onChanged: list.reload });

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

  const navigateTo = React.useCallback(
    (id: string | null) => {
      router.push(id ? `/drive?folder=${id}` : "/drive", { scroll: false });
    },
    [router],
  );
  // スマホ左上の「戻る」。親はパンくずの末尾の 1 つ手前（無ければドライブ直下）。
  const parentId = crumbs.length >= 2 ? crumbs[crumbs.length - 2].id : null;
  const goParent = React.useCallback(() => navigateTo(parentId), [navigateTo, parentId]);
  const back = React.useMemo(
    () => (folderId ? { onClick: goParent, label: "親フォルダへ戻る" } : undefined),
    [folderId, goParent],
  );

  const onDrop = (e: React.DragEvent) => {
    e.preventDefault();
    setDragging(false);
    void uploader.uploadFiles(Array.from(e.dataTransfer.files));
  };

  const openUpload = () => {
    afterPickRef.current = null;
    fileInputRef.current?.click();
  };
  const openUploadFromSheet = (closeSheet: () => void) => {
    afterPickRef.current = closeSheet;
    fileInputRef.current?.click();
  };
  const openNewFolder = () => {
    setActiveNode(null);
    setDialog("newfolder");
  };

  // --- 行アクション ---
  const handleAction = (action: NodeAction, node: NodeResponse) => {
    switch (action) {
      case "open":
        // フォルダは配下をブラウズ。Office 文書は編集セッションを確かめてから Collabora へ。
        // それ以外は resource-link の拡張子→ルート対応に従う（ノート・CSV・スライド・PDF の
        // 各ビューア、ビューアの無い形式は「プレビューできません」画面）。開くでダウンロードはしない。
        if (node.kind === "folder") navigateTo(node.id);
        else if (isOfficeFile(node.name)) void openOffice(node);
        else router.push(resourcePath(node));
        break;
      case "download":
        void download(node);
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

  const currentName = crumbs.length > 0 ? crumbs[crumbs.length - 1].name : null;
  const countLabel = !list.loading && list.items.length > 0
    ? `${list.items.length}${list.hasMore ? "+" : ""} 件`
    : null;

  const searchCounts =
    searching && !list.loading && !contentSearch.loading && contentRows.length + list.items.length > 0
      ? [
          contentRows.length > 0 ? `内容一致 ${contentRows.length} 件` : null,
          list.items.length > 0 ? `名前一致 ${list.items.length}${list.hasMore ? "+" : ""} 件` : null,
        ]
          .filter(Boolean)
          .join("・")
      : null;

  return (
    <div className="flex flex-col gap-4 max-md:gap-3">
      <DriveChrome
        back={back}
        search={{ open: searchVisible, onToggle: toggleMobileSearch }}
      />

      {/* スマホの見出し（タイトルバーの代わり）。右に並べ替えと表示切替を 1 段で置く。
          フォルダの中では上に短いパンくず（親へ戻る＋上の階層の …）と件数を出す。 */}
      {folderId ? (
        <div className="-mb-2 flex min-h-8 items-center justify-between gap-3 text-[12.5px] md:hidden">
          <MobileBreadcrumbs crumbs={crumbs} onNavigate={navigateTo} />
          {countLabel ? (
            <span className="shrink-0 tabular-nums text-muted-foreground">{countLabel}</span>
          ) : null}
        </div>
      ) : null}
      <MobilePageTitle
        className="px-0 pb-0"
        eyebrow={folderId ? undefined : `マイドライブ${countLabel ? `・${countLabel}` : ""}`}
        title={
          <span className="line-clamp-2 [overflow-wrap:anywhere]">
            {folderId ? (currentName ?? "…") : "ドライブ"}
          </span>
        }
      >
        <div className="flex shrink-0 items-center gap-1 self-end">
          <SortMenu
            sort={sort}
            desc={desc}
            onSortChange={(f) => setSort(f)}
            onDescChange={setDesc}
          />
          <ViewToggle view={view} onChange={changeView} />
        </div>
      </MobilePageTitle>

      {/* コマンドバー（PC）: 左=新規作成＋操作＋並べ替え＋表示切替 / 右=検索。高さ(h-9)・角丸(lg)を揃える。
          スマホは検索欄だけを、右上の検索アイコンで開いた時に出す。 */}
      <div
        className={cn(
          "flex flex-wrap items-center gap-2.5",
          !searchVisible && "max-md:hidden",
        )}
      >
        <div className="contents max-md:hidden">
          {/* 新規作成（丸＋）。ノート/ドキュメント/スライド/スプレッドシート/CSV。 */}
          <NewMenuDesktop create={create} />
          <Button variant="outline" className="rounded-lg" onClick={openNewFolder}>
            <FolderPlus className="size-4" aria-hidden />
            新規フォルダ
          </Button>
          <Button className="rounded-lg" onClick={openUpload}>
            <Upload className="size-4" aria-hidden />
            アップロード
          </Button>

          <div className="mx-0.5 h-6 w-px bg-border/70" aria-hidden />

          <SortMenu sort={sort} desc={desc} onSortChange={(f) => setSort(f)} onDescChange={setDesc} />
          <ViewToggle view={view} onChange={changeView} />
        </div>

        {/* 検索（右寄せ） */}
        <div className="relative w-full md:ml-auto md:w-72">
          <Search
            className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground/70"
            aria-hidden
          />
          <Input
            ref={searchInputRef}
            type="search"
            enterKeyHint="search"
            value={searchInput}
            onChange={(e) => setSearchInput(e.target.value)}
            onKeyDown={(e) => {
              // 確定でキーボードを閉じる（結果は打鍵に追従して出ているため送信はしない）。
              if (e.key === "Enter") (e.target as HTMLInputElement).blur();
            }}
            placeholder="ドライブを検索"
            aria-label="ドライブを検索"
            className="h-9 rounded-lg pl-9 pr-9 max-md:h-11"
          />
          {searchInput ? (
            <button
              type="button"
              onClick={() => {
                setSearchInput("");
                searchInputRef.current?.focus();
              }}
              aria-label="検索をクリア"
              className="absolute right-1.5 top-1/2 grid size-7 -translate-y-1/2 place-items-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground pointer-coarse:size-9"
            >
              <X className="size-4" aria-hidden />
            </button>
          ) : null}
        </div>
      </div>

      {/* 隠しファイル入力（アップロード／新バージョン）。ツールバーの外に置き、スマホで
          ツールバーを隠しても FAB のシートから開けるようにする。 */}
      <input
        ref={fileInputRef}
        type="file"
        multiple
        hidden
        onChange={(e) => {
          const files = Array.from(e.target.files ?? []);
          e.target.value = "";
          if (files.length === 0) return;
          // スマホの「＋ 新規」のシートはファイルを選ぶまで開いておき、選んだら閉じる。
          afterPickRef.current?.();
          afterPickRef.current = null;
          void uploader.uploadFiles(files);
        }}
      />
      <input
        ref={versionInputRef}
        type="file"
        hidden
        onChange={(e) => {
          const file = e.target.files?.[0];
          const node = versionTargetRef.current;
          if (file && node) void uploader.uploadNewVersion(file, node);
          versionTargetRef.current = null;
          e.target.value = "";
        }}
      />

      {/* 現在地（パンくず/検索結果）と件数。パンくずはサブフォルダにいる時だけ
          （ルートはシェルの見出し「ドライブ」と重複するため出さない）。スマホは見出しが
          現在地と件数を出すので、検索中だけこの行を出す。 */}
      {searching || crumbs.length > 0 || countLabel ? (
        <div
          className={cn(
            "flex min-h-7 items-center justify-between gap-x-3 gap-y-1 px-1",
            // 検索中だけ折り返す（件数をクエリの下へ回す）。パンくずは 1 行で省略する。
            searching ? "flex-wrap" : "max-md:hidden",
          )}
        >
          <div className="min-w-0">
            {searching ? (
              // 長いクエリでも「詳細検索」が切れないよう、クエリだけを省略する。
              <p className="flex min-w-0 flex-wrap items-baseline gap-x-2 text-sm text-muted-foreground">
                <span className="min-w-0 max-w-full truncate">「{query}」の検索結果</span>
                <a
                  href={`/search?q=${encodeURIComponent(query)}`}
                  className="relative shrink-0 text-xs text-primary underline-offset-2 hover:underline pointer-coarse:py-1.5"
                >
                  詳細検索（引用・絞込の内訳）
                </a>
              </p>
            ) : crumbs.length > 0 ? (
              <Breadcrumbs crumbs={crumbs} onNavigate={navigateTo} />
            ) : null}
          </div>
          {searching ? (
            searchCounts ? (
              <span className="shrink-0 text-[13px] tabular-nums text-muted-foreground">{searchCounts}</span>
            ) : null
          ) : countLabel ? (
            <span className="shrink-0 text-[13px] tabular-nums text-muted-foreground">{countLabel}</span>
          ) : null}
        </div>
      ) : null}

      {/* アップロード進捗（1 件ごとの一意 ID で管理） */}
      {uploader.uploads.length > 0 ? (
        <div
          className="flex flex-col gap-2 rounded-lg border border-border bg-card p-3"
          aria-live="polite"
          aria-label="アップロード中"
        >
          {uploader.uploads.map((u) => (
            <div key={u.id} className="flex items-center gap-3">
              <Loader2 className="size-4 shrink-0 animate-spin text-primary" aria-hidden />
              <span className="min-w-0 flex-1 truncate text-sm">{u.name}</span>
              <div className="h-1.5 w-28 shrink-0 overflow-hidden rounded-full bg-secondary max-sm:w-20">
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
          // @container: 列の出し入れを**一覧自身の幅**で決める（LIST_GRID）。ビューポート幅で
          // 決めると、サイドバーの出現（768px で 0→288px）の分だけ一覧が狭くなり、
          // 同じ幅なのに名前列だけが潰れる（#466）。
          "@container rule-soft relative min-h-[16rem] rounded-xl transition-colors",
          // スマホは行の余白（px-3）の分だけ外へ広げ、行の中身を見出しと同じ 16px の線に揃える。
          "max-md:-mx-3",
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
            contentSearch.loading ? (
              <LoadingRow />
            ) : contentSearch.hits.length === 0 ? (
              <EmptyState
                icon={Search}
                className="max-md:mx-3"
                title="見つかりませんでした"
                description={`「${query}」に名前・内容が一致するファイル・フォルダはありません。`}
              />
            ) : null
          ) : (
            <EmptyFolder onUpload={openUpload} onNewFolder={openNewFolder} />
          )
        ) : view === "grid" ? (
          <div className="grid grid-cols-2 gap-3 p-1 max-md:px-3 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5">
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
              <NodeRow key={node.id} node={node} onAction={handleAction} meId={me.data?.id} />
            ))}
            {list.hasMore ? <div ref={sentinelRef}>{list.loadingMore ? <LoadingRow /> : null}</div> : null}
          </div>
        )}
      </div>

      {/* スマホの「＋ 新規」（アップロード／新規フォルダ／新規作成を 1 つに集約） */}
      <NewFab
        create={create}
        onUpload={openUploadFromSheet}
        onNewFolder={openNewFolder}
      />

      {/* ダイアログ群 */}
      <UploadConflictDialog
        conflicts={uploader.conflicts}
        onResolve={(r) => void uploader.resolveConflicts(r)}
      />

      <TextPromptDialog
        open={dialog === "newfolder"}
        onOpenChange={(o) => (o ? null : closeDialog())}
        title="新規フォルダ"
        label="フォルダ名"
        submitLabel="作成"
        onSubmit={async (name) => {
          try {
            await createFolder(name, folderId ?? undefined);
          } catch (e) {
            // 入力欄の下に出る。番号だけのエラーにしない。
            if (isConflict(e)) throw new Error("同じ名前のフォルダがあります");
            throw e;
          }
          list.reload();
        }}
      />

      <TextPromptDialog
        open={dialog === "rename"}
        onOpenChange={(o) => (o ? null : closeDialog())}
        title="名前を変更"
        label="新しい名前"
        initialValue={activeNode?.name ?? ""}
        // ファイルは拡張子を変えさせない（開く画面と中身がずれる・#617）。名前の部分だけを選んでおく。
        initialSelection={
          activeNode && activeNode.kind !== "folder" ? stemRange(activeNode.name) : undefined
        }
        validate={
          activeNode && activeNode.kind !== "folder"
            ? (v) => extensionChangeError(activeNode.name, v)
            : undefined
        }
        submitLabel="変更"
        onSubmit={async (name) => {
          if (!activeNode) return;
          try {
            if (activeNode.kind === "folder") await updateFolder(activeNode.id, { name });
            else await updateFile(activeNode.id, { name });
          } catch (e) {
            if (isConflict(e)) throw new Error("この場所に同じ名前のファイルまたはフォルダがあります");
            throw e;
          }
          list.reload();
        }}
      />

      <MoveDialog
        open={dialog === "move"}
        onOpenChange={(o) => (o ? null : closeDialog())}
        node={activeNode}
        onMove={async (dest) => {
          if (!activeNode) return;
          try {
            if (activeNode.kind === "folder") await updateFolder(activeNode.id, { move: dest });
            else await updateFile(activeNode.id, { move: dest });
          } catch (e) {
            if (isConflict(e)) throw new Error("移動先に同じ名前のファイルまたはフォルダがあります");
            throw e;
          }
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

/// 空フォルダ。マウスの端末は D&D を案内し、タッチ（D&D が無い）はその場で追加できる
/// ボタンを出す（スマホでは「＋ 新規」と同じ操作）。
function EmptyFolder({ onUpload, onNewFolder }: { onUpload: () => void; onNewFolder: () => void }) {
  return (
    <EmptyState
      icon={UploadCloud}
      title="このフォルダは空です"
      className="max-md:mx-3"
      description={undefined}
      action={
        <div className="flex flex-col items-center gap-3">
          <p className="max-w-sm text-sm text-muted-foreground pointer-coarse:hidden">
            ファイルをドラッグ＆ドロップするか、アップロードボタンから追加できます。
          </p>
          <p className="hidden max-w-sm text-sm text-muted-foreground pointer-coarse:block">
            ファイルをアップロードするか、フォルダを作って整理できます。
          </p>
          <div className="hidden flex-wrap justify-center gap-2 pointer-coarse:flex">
            <Button variant="outline" onClick={onNewFolder}>
              <FolderPlus className="size-4" aria-hidden />
              新規フォルダ
            </Button>
            <Button onClick={onUpload}>
              <Upload className="size-4" aria-hidden />
              アップロード
            </Button>
          </div>
        </div>
      }
    />
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
          className="shiki-dash-bottom flex w-full items-start gap-3 px-3 py-2.5 text-left outline-none transition-colors hover:bg-accent focus-visible:bg-accent active:bg-accent"
        >
          <FileText className="mt-0.5 size-[18px] shrink-0 text-primary" aria-hidden />
          <span className="min-w-0 flex-1">
            <span className="block truncate text-sm font-medium text-foreground">{h.fileName}</span>
            <span className="mt-0.5 line-clamp-2 text-xs text-muted-foreground md:line-clamp-1">
              {h.snippet}
            </span>
          </span>
        </button>
      ))}
    </div>
  );
}
