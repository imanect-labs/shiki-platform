"use client";

import * as React from "react";

import {
  listArtifacts,
  listMoreSharedArtifacts,
  mergeArtifacts,
  type ArtifactKind,
  type ArtifactMeta,
} from "@/lib/artifact-api";

/// スキル・アプリの一覧の状態（[`useArtifactList`]）。
export type ArtifactListState = {
  /// 読めた一覧（読み込み中は `null`）。
  items: ArtifactMeta[] | null;
  /// 最初の読み込みが失敗した（空の一覧に見せない・付録 C の P5）。
  error: boolean;
  /// 共有された分の続きの位置（読み切ったら `null`）。
  sharedCursor: string | null;
  /// 共有された分の続きの頼みが失敗した。
  sharedFailed: boolean;
  /// 共有された分の続きを読んでいる。
  loadingMore: boolean;
  reload: () => void;
  /// 共有された分の続きを読む（[`listMoreSharedArtifacts`]）。
  loadMore: () => void;
};

/// アーティファクトの一覧を読み、共有された分を読み切れなかったら続きを押して読めるようにする（BUG-162）。
///
/// 共有されたものが多い人では、共有された分が 0 件のまま続きの位置だけが付いて返ることがある。読み切れなかったこと
/// と、続きの頼みの失敗は、画面が「さらに読み込む」で言う（黙って欠かない）。
export function useArtifactList(kind: ArtifactKind): ArtifactListState {
  const [items, setItems] = React.useState<ArtifactMeta[] | null>(null);
  const [error, setError] = React.useState(false);
  const [sharedCursor, setSharedCursor] = React.useState<string | null>(null);
  const [sharedFailed, setSharedFailed] = React.useState(false);
  const [loadingMore, setLoadingMore] = React.useState(false);
  // 読み直しの後に、前の読み込みの続きが届いても混ぜない。
  const generation = React.useRef(0);

  const reload = React.useCallback(() => {
    const gen = ++generation.current;
    setError(false);
    setLoadingMore(false);
    listArtifacts(kind)
      .then((list) => {
        if (gen !== generation.current) return;
        setItems(list.items);
        setSharedCursor(list.sharedCursor);
        setSharedFailed(list.sharedFailed);
      })
      .catch(() => {
        if (gen !== generation.current) return;
        setItems([]);
        setSharedCursor(null);
        setSharedFailed(false);
        setError(true);
      });
  }, [kind]);
  React.useEffect(reload, [reload]);

  const loadMore = React.useCallback(() => {
    if (!sharedCursor || loadingMore) return;
    const gen = generation.current;
    setLoadingMore(true);
    listMoreSharedArtifacts(kind, sharedCursor)
      .then((list) => {
        if (gen !== generation.current) return;
        setItems((prev) => mergeArtifacts(prev ?? [], list.items));
        setSharedCursor(list.sharedCursor);
        setSharedFailed(list.sharedFailed);
      })
      .finally(() => {
        if (gen === generation.current) setLoadingMore(false);
      });
  }, [kind, sharedCursor, loadingMore]);

  return { items, error, sharedCursor, sharedFailed, loadingMore, reload, loadMore };
}
