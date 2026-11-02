"use client";

import * as React from "react";
import { ArrowUpRight, FileText, Folder, Search } from "lucide-react";

import {
  mergeSearchResults,
  useContentSearch,
  useNameSearch,
  type DriveSearchItem,
} from "@/lib/drive-search";

/// ⌘K パレット用のドライブ横断検索（名前一致＋内容一致の統合・スコア降順）。
/// クエリはここでデバウンスする（呼び出し側は生の入力を渡す）。
export function useDriveSearch(query: string, enabled: boolean) {
  const [debounced, setDebounced] = React.useState("");
  React.useEffect(() => {
    const t = setTimeout(() => setDebounced(query.trim()), 300);
    return () => clearTimeout(t);
  }, [query]);

  const name = useNameSearch(debounced, enabled);
  const content = useContentSearch(debounced, enabled);
  const items = React.useMemo(
    () => mergeSearchResults(name.nodes, content.hits, 8),
    [name.nodes, content.hits],
  );
  // 名前で探しきれていない（続きがある）。名前一致が無いように見せない。
  const incomplete = name.incomplete && !name.loading;
  return { items, incomplete, loading: name.loading || content.loading };
}

/// パレット内の「ドライブ」セクション。フォルダ/ファイルをスコアの高い順に並べる。
export function DriveResults({
  query,
  items,
  loading,
  incomplete = false,
  onOpen,
  onSearchMore,
}: {
  query: string;
  items: DriveSearchItem[];
  loading: boolean;
  /// 名前一致を探しきれていない（ドライブの検索で続きを探せる・BUG-187）。
  incomplete?: boolean;
  onOpen: (item: DriveSearchItem) => void;
  /// ドライブの検索の画面で続きを探す。
  onSearchMore?: () => void;
}) {
  if (!query.trim()) return null;
  if (!loading && items.length === 0 && !incomplete) return null;

  return (
    <div className="mt-1" aria-label="ドライブの検索結果">
      <div className="px-3 pb-1 pt-2 text-[11px] font-medium text-muted-foreground/70">
        ドライブ
      </div>
      {loading && items.length === 0 ? (
        <p className="px-3 py-2 text-sm text-muted-foreground">検索中...</p>
      ) : items.length === 0 ? (
        <p className="px-3 py-2 text-sm text-muted-foreground">まだ見つかっていません。</p>
      ) : (
        <ul>
          {items.map((item) => (
            <li key={item.id}>
              <button
                type="button"
                onClick={() => onOpen(item)}
                className="flex w-full items-start gap-3 rounded-lg px-3 py-2 text-left outline-none transition-colors hover:bg-accent focus-visible:bg-accent"
              >
                {item.kind === "folder" ? (
                  <Folder
                    className="mt-0.5 size-[18px] shrink-0 fill-amber-400 text-amber-500"
                    aria-hidden
                  />
                ) : (
                  <FileText
                    className="mt-0.5 size-[18px] shrink-0 text-muted-foreground"
                    aria-hidden
                  />
                )}
                <span className="min-w-0">
                  <span className="block truncate text-sm font-medium text-foreground">
                    {item.name}
                  </span>
                  {item.snippet ? (
                    <span className="mt-0.5 line-clamp-1 text-xs text-muted-foreground">
                      {item.snippet}
                    </span>
                  ) : null}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
      {incomplete && onSearchMore ? (
        <button
          type="button"
          data-testid="palette-drive-more"
          onClick={onSearchMore}
          className="flex w-full items-center gap-3 rounded-lg px-3 py-2 text-left text-sm text-foreground outline-none transition-colors hover:bg-accent focus-visible:bg-accent active:bg-accent max-sm:min-h-12"
        >
          <Search className="size-[18px] shrink-0 text-muted-foreground" aria-hidden />
          <span className="min-w-0">ドライブで続きを探す</span>
          <ArrowUpRight className="ml-auto size-4 shrink-0 text-muted-foreground" aria-hidden />
        </button>
      ) : null}
    </div>
  );
}
