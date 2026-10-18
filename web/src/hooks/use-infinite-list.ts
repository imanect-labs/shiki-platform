"use client";

import * as React from "react";

/// カーソルページングの 1 ページ（サーバ応答の共通形）。
export type Page<T> = { items: T[]; next_cursor?: string | null };

export type InfiniteList<T> = {
  items: T[];
  loading: boolean;
  /// 追加ページ取得中（無限スクロールのフッタ表示用）。
  loadingMore: boolean;
  error: string | null;
  hasMore: boolean;
  /// 次ページを取得する（無限スクロールの sentinel から呼ぶ）。
  loadMore: () => void;
  /// 先頭から取り直す（作成/削除/リネーム後の反映）。
  reload: () => void;
};

/// `next_cursor` を消費する汎用の無限スクロールフック。
///
/// `fetchPage(cursor)` は 1 ページを返す。`deps` が変わると先頭から読み直す
/// （フォルダ移動・ソート変更・検索語変更など）。全件取得はしない。
export function useInfiniteList<T>(
  fetchPage: (cursor?: string) => Promise<Page<T>>,
  deps: React.DependencyList,
): InfiniteList<T> {
  const [items, setItems] = React.useState<T[]>([]);
  const [cursor, setCursor] = React.useState<string | undefined>(undefined);
  const [hasMore, setHasMore] = React.useState(true);
  const [loading, setLoading] = React.useState(true);
  const [loadingMore, setLoadingMore] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const [reloadKey, setReloadKey] = React.useState(0);

  // 競合防止: 最新リクエストのみ反映する世代カウンタ。
  const generation = React.useRef(0);
  // 進行中フラグ（同一 sentinel の多重発火を抑止）。
  const fetching = React.useRef(false);

  const fetchPageRef = React.useRef(fetchPage);
  fetchPageRef.current = fetchPage;

  // 先頭ロード（deps / reload で発火）。
  React.useEffect(() => {
    const gen = ++generation.current;
    setItems([]);
    setCursor(undefined);
    setHasMore(true);
    setLoading(true);
    setError(null);
    fetching.current = true;
    fetchPageRef
      .current(undefined)
      .then((page) => {
        if (gen !== generation.current) return;
        setItems(page.items);
        setCursor(page.next_cursor ?? undefined);
        setHasMore(Boolean(page.next_cursor));
      })
      .catch((e: unknown) => {
        if (gen !== generation.current) return;
        setError(e instanceof Error ? e.message : String(e));
        setHasMore(false);
      })
      .finally(() => {
        if (gen !== generation.current) return;
        setLoading(false);
        fetching.current = false;
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, reloadKey]);

  const loadMore = React.useCallback(() => {
    if (fetching.current || !hasMore || !cursor) return;
    const gen = generation.current;
    fetching.current = true;
    setLoadingMore(true);
    fetchPageRef
      .current(cursor)
      .then((page) => {
        if (gen !== generation.current) return;
        setItems((prev) => [...prev, ...page.items]);
        setCursor(page.next_cursor ?? undefined);
        setHasMore(Boolean(page.next_cursor));
      })
      .catch((e: unknown) => {
        if (gen !== generation.current) return;
        setError(e instanceof Error ? e.message : String(e));
        setHasMore(false);
      })
      .finally(() => {
        if (gen !== generation.current) return;
        setLoadingMore(false);
        fetching.current = false;
      });
  }, [cursor, hasMore]);

  const reload = React.useCallback(() => setReloadKey((k) => k + 1), []);

  return { items, loading, loadingMore, error, hasMore, loadMore, reload };
}

/// 無限スクロールの sentinel を監視し、可視になったら `onVisible` を呼ぶ。
///
/// IntersectionObserver は「見え方が変わったとき」しか発火しないため、読み足した行が
/// 呼び出し側で絞り込まれて画面が埋まらない（sentinel が見えたまま）と続きが止まる。
/// `progress`（読み込み済み件数など、取得成功で増える値。取得中は `undefined`）を渡すと、
/// 値が変わった時点で sentinel がまだ見えていれば `onVisible` をもう一度呼ぶ。
/// 失敗で値が変わらなければ再発火しない（失敗の連打を防ぐ。見え直したときに再試行）。
export function useInfiniteSentinel(
  onVisible: () => void,
  enabled: boolean,
  progress?: number,
): React.RefObject<HTMLDivElement | null> {
  const ref = React.useRef<HTMLDivElement | null>(null);
  const cb = React.useRef(onVisible);
  cb.current = onVisible;
  const visible = React.useRef(false);
  const lastProgress = React.useRef<number | undefined>(undefined);

  React.useEffect(() => {
    const node = ref.current;
    if (!node || !enabled) return;
    const observer = new IntersectionObserver(
      (entries) => {
        visible.current = entries[entries.length - 1]?.isIntersecting ?? false;
        if (entries.some((e) => e.isIntersecting)) cb.current();
      },
      { rootMargin: "200px" },
    );
    observer.observe(node);
    return () => {
      observer.disconnect();
      visible.current = false;
    };
  }, [enabled]);

  // 取得完了で件数が変わり、まだ sentinel が見えていれば続きを読む（1 回に 1 ページ）。
  React.useEffect(() => {
    if (progress === undefined) return;
    const changed = lastProgress.current !== undefined && lastProgress.current !== progress;
    lastProgress.current = progress;
    if (changed && enabled && visible.current) cb.current();
  }, [progress, enabled]);

  return ref;
}
