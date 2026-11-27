"use client";

import * as React from "react";

import {
  mergeNewest,
  readFirst,
  readMore,
  type FetchPage,
  type Listed,
  type MoreFailed,
  type OwnedPosition,
} from "@/lib/owned-shared-list";

/// 自分のものと共有されたものを合わせた一覧の状態（[`useOwnedSharedList`]）。
export type OwnedSharedListState<T> = {
  /// 読めた一覧（読み込み中は `null`）。
  items: T[] | null;
  /// 最初の読み込みが失敗した（空の一覧に見せない・付録 C の P5）。
  error: boolean;
  /// 自分のものの続きの位置（読み切ったら `null`）。
  ownedNext: OwnedPosition | null;
  /// 共有された分の続きの位置（読み切ったら `null`）。
  sharedCursor: string | null;
  /// 続きの頼みが失敗した（失敗した側の位置は残す）。
  failed: MoreFailed;
  /// 続きを読んでいる。
  loadingMore: boolean;
  /// まだ読んでいない続きがある。
  hasMore: boolean;
  reload: () => void;
  /// 残っている続きを読む（自分のものと共有された分の両方）。
  loadMore: () => void;
  /// 一覧から 1 件を外す（削除の後・読み込み済みの続きを捨てない）。
  remove: (id: string) => void;
};

/// 自分のものと共有されたものを合わせた一覧を読み、続きを押して読めるようにする（BUG-162・BUG-185・BUG-186）。
///
/// `fetchPage` は描画のたびに作り直さない（`useMemo`・`useCallback` か、モジュールの関数を渡す）。変わると先頭から
/// 読み直す。`enabled` が偽のあいだは読まず、真になるたびに先頭から読み直す（開いたときだけ読むダイアログ）。
export function useOwnedSharedList<T extends Listed>(
  fetchPage: FetchPage<T>,
  { enabled = true }: { enabled?: boolean } = {},
): OwnedSharedListState<T> {
  const [items, setItems] = React.useState<T[] | null>(null);
  const [error, setError] = React.useState(false);
  const [ownedNext, setOwnedNext] = React.useState<OwnedPosition | null>(null);
  const [sharedCursor, setSharedCursor] = React.useState<string | null>(null);
  const [failed, setFailed] = React.useState<MoreFailed>(null);
  const [loadingMore, setLoadingMore] = React.useState(false);
  // 読み直しの後に、前の読み込みの続きが届いても混ぜない。
  const generation = React.useRef(0);
  // 失敗の後の読み直しは「読み込み中」に戻す（空の一覧を一瞬でも見せない）。
  const failedFirst = React.useRef(false);

  const reload = React.useCallback(() => {
    const gen = ++generation.current;
    if (failedFirst.current) setItems(null);
    failedFirst.current = false;
    setError(false);
    setLoadingMore(false);
    readFirst(fetchPage)
      .then((list) => {
        if (gen !== generation.current) return;
        setItems(list.items);
        setOwnedNext(list.ownedNext);
        setSharedCursor(list.sharedCursor);
        setFailed(list.failed);
      })
      .catch(() => {
        if (gen !== generation.current) return;
        failedFirst.current = true;
        setItems([]);
        setOwnedNext(null);
        setSharedCursor(null);
        setFailed(null);
        setError(true);
      });
  }, [fetchPage]);
  React.useEffect(() => {
    if (enabled) reload();
  }, [reload, enabled]);

  const loadMore = React.useCallback(() => {
    if ((!ownedNext && !sharedCursor) || loadingMore) return;
    const gen = generation.current;
    setLoadingMore(true);
    readMore(fetchPage, { ownedNext, sharedCursor })
      .then((list) => {
        if (gen !== generation.current) return;
        setItems((prev) => mergeNewest(prev ?? [], list.items));
        setOwnedNext(list.ownedNext);
        setSharedCursor(list.sharedCursor);
        setFailed(list.failed);
      })
      .finally(() => {
        if (gen === generation.current) setLoadingMore(false);
      });
  }, [fetchPage, ownedNext, sharedCursor, loadingMore]);

  const remove = React.useCallback((id: string) => {
    setItems((prev) => (prev ?? []).filter((x) => x.id !== id));
  }, []);

  return {
    items,
    error,
    ownedNext,
    sharedCursor,
    failed,
    loadingMore,
    hasMore: ownedNext !== null || sharedCursor !== null,
    reload,
    loadMore,
    remove,
  };
}
