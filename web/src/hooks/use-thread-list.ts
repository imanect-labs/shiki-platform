"use client";

import * as React from "react";

import { listThreads, subscribeThreadsChanged, type Thread } from "@/lib/chat-api";

export type ThreadList = {
  threads: Thread[];
  /// 先頭ページの取得前（スケルトン用。読み直しでは立てない）。
  loading: boolean;
  /// 続きのページを取得中（一覧末尾のインジケータ用）。
  loadingMore: boolean;
  /// 先頭ページの取得に失敗した（「該当なし」と区別して出すため）。
  error: boolean;
  hasMore: boolean;
  /// 続きを読み足す（無限スクロールの sentinel から呼ぶ）。
  loadMore: () => void;
  /// 手元の行を書き換える（`null` は取り除く）。改名・削除の直後に使う: 通知での取り直しは
  /// 先頭ページだけなので、それより古い行は書き換えないと古い表示のまま残る。
  patchLocal: (id: string, patch: Partial<Thread> | null) => void;
  /// 手元に無い行を並び順の位置へ足す（あれば置き換える）。ピン留めを外した行を一覧へ戻すのに使う。
  /// 読み込み済みの範囲（続きがあるなら末尾の行より新しい行）に入るものだけ足す。それより古い行は
  /// 続きの読み足しで届くので、ここで足すと並び（keyset 順）が崩れる。
  upsertLocal: (thread: Thread) => void;
};

/// 生成中（順番待ち・生成中）の会話があるときの読み直し間隔。終わったらすぐ「完了」に変えたい。
const LIVE_POLL_MS = 4_000;
/// 承認待ちだけのときの読み直し間隔（承認は別の画面から返ることもあるが、急がない）。
const WAITING_POLL_MS = 20_000;

/// 裏で生成が続いている会話があれば、先頭ページを定期的に読み直す（実行中→完了の切り替え・
/// 終わった会話の先頭への移動を、その会話を開いていなくても拾う）。
///
/// 終わりを知らせる通知はその会話の画面（SSE）にしか届かないため、ここは読み直しで追う。
/// 実行中の会話が無ければ何もしない。タブが裏にある間は叩かず、表に戻ったら 1 度読む。
function useLiveRefresh(threads: Thread[], refresh: React.RefObject<(() => void) | null>) {
  const live = threads.some((t) => t.runStatus === "queued" || t.runStatus === "running");
  const waiting = !live && threads.some((t) => t.runStatus === "waiting_approval");
  React.useEffect(() => {
    if (!live && !waiting) return;
    const tick = () => {
      if (document.visibilityState === "visible") refresh.current?.();
    };
    const id = window.setInterval(tick, live ? LIVE_POLL_MS : WAITING_POLL_MS);
    document.addEventListener("visibilitychange", tick);
    return () => {
      window.clearInterval(id);
      document.removeEventListener("visibilitychange", tick);
    };
  }, [live, waiting, refresh]);
}

/// `(updated_at, id)` の降順で a が b より後ろ（古い）か。サーバの keyset 順と一致させる。
function isOlder(a: Thread, b: Thread): boolean {
  if (a.updatedAt !== b.updatedAt) return a.updatedAt < b.updatedAt;
  return a.id < b.id;
}

/// 自分のスレッド一覧を `next_cursor` で読み足しながら購読する（#591）。
///
/// - `query` を渡すとタイトルの部分一致でサーバ側が全件から絞る。変わると先頭から読み直す。
/// - 作成・更新の通知では**先頭ページだけ取り直して差し替え**、読み込み済みの続きは保つ
///   （送信のたびに一覧が先頭へ巻き戻ったりスケルトンがちらついたりしないように）。
///   先頭ページより古い行は keyset 順でそのまま有効なので、先頭ページに出た id と、
///   先頭ページの範囲内なのに出てこなかった行（削除・更新で前へ移った行）だけを落とす。
/// - `enabled` が false の間は取得しない（閉じているパレット等）。
export function useThreadList(query = "", enabled = true): ThreadList {
  const [threads, setThreads] = React.useState<Thread[]>([]);
  const [cursor, setCursor] = React.useState<string | null>(null);
  const [loading, setLoading] = React.useState(true);
  const [loadingMore, setLoadingMore] = React.useState(false);
  const [error, setError] = React.useState(false);
  // 通知での差し替えは「今表示している行」を基準に計算する（state 更新関数の中で副作用を起こさない）。
  const threadsRef = React.useRef<Thread[]>([]);
  threadsRef.current = threads;

  // 検索語の切り替えで古い応答が後から届いても反映しない世代カウンタ。
  const generation = React.useRef(0);
  const fetchingMore = React.useRef(false);
  const refresh = React.useRef<(() => void) | null>(null);
  useLiveRefresh(threads, refresh);

  React.useEffect(() => {
    if (!enabled) return;
    const gen = ++generation.current;
    // 後片付け（検索語の切り替え・無効化・アンマウント）後に届いた応答を捨てる。
    let active = true;
    const current = () => active && gen === generation.current;
    fetchingMore.current = false;
    setLoading(true);
    setLoadingMore(false);
    setError(false);
    setThreads([]);
    setCursor(null);

    // 重なった取り直し（実行中の読み直しと変更通知）は、最後に出した要求の応答だけを反映する
    // （古い「実行中なし」が新しい「実行中」を後から上書きすると、読み直しが止まる）。
    let latest = 0;
    const loadFirst = (initial: boolean) => {
      const seq = ++latest;
      listThreads(undefined, { q: query })
        .then(({ threads: first, nextCursor }) => {
          if (!current() || seq !== latest) return;
          if (initial) {
            setError(false);
            setThreads(first);
            setCursor(nextCursor);
            return;
          }
          const ids = new Set(first.map((t) => t.id));
          const edge = first[first.length - 1];
          const tail =
            nextCursor && edge
              ? threadsRef.current.filter((t) => !ids.has(t.id) && isOlder(t, edge))
              : [];
          setThreads([...first, ...tail]);
          // 続きを読み込んでいなければ（tail が空）カーソルは先頭ページのものに揃える。
          // 読み込み済みなら、手元の末尾を指す今のカーソルがそのまま有効。
          if (tail.length === 0) setCursor(nextCursor);
        })
        .catch(() => {
          if (!current() || seq !== latest) return;
          // 通知での取り直しの失敗は、表示中の一覧をそのまま残す。
          if (initial) {
            setThreads([]);
            setCursor(null);
            setError(true);
          }
        })
        .finally(() => {
          if (current() && initial) setLoading(false);
        });
    };

    loadFirst(true);
    refresh.current = () => loadFirst(false);
    const unsubscribe = subscribeThreadsChanged(() => loadFirst(false));
    return () => {
      unsubscribe();
      refresh.current = null;
      active = false;
    };
  }, [query, enabled]);

  const loadMore = React.useCallback(() => {
    if (!cursor || fetchingMore.current) return;
    const gen = generation.current;
    fetchingMore.current = true;
    setLoadingMore(true);
    listThreads(cursor, { q: query })
      .then(({ threads: page, nextCursor }) => {
        if (gen !== generation.current) return;
        setThreads((prev) => {
          const seen = new Set(prev.map((t) => t.id));
          return [...prev, ...page.filter((t) => !seen.has(t.id))];
        });
        setCursor(nextCursor);
      })
      .catch(() => {
        // 続きの取得失敗は一覧を壊さない（カーソルは残し、sentinel が再び見えたときに再試行する）。
      })
      .finally(() => {
        if (gen !== generation.current) return;
        fetchingMore.current = false;
        setLoadingMore(false);
      });
  }, [cursor, query]);

  const patchLocal = React.useCallback((id: string, patch: Partial<Thread> | null) => {
    setThreads((prev) =>
      patch === null
        ? prev.filter((t) => t.id !== id)
        : prev.map((t) => (t.id === id ? { ...t, ...patch } : t)),
    );
  }, []);

  const hasMore = cursor !== null;
  const upsertLocal = React.useCallback(
    (thread: Thread) => {
      setThreads((prev) => {
        const rest = prev.filter((t) => t.id !== thread.id);
        const edge = rest[rest.length - 1];
        if (hasMore && edge && isOlder(thread, edge)) return rest;
        const at = rest.findIndex((t) => isOlder(t, thread));
        return at < 0 ? [...rest, thread] : [...rest.slice(0, at), thread, ...rest.slice(at)];
      });
    },
    [hasMore],
  );

  return { threads, loading, loadingMore, error, hasMore, loadMore, patchLocal, upsertLocal };
}

/// ピン留めしたスレッド（ピン留めの新しい順・全件）。一覧の変更通知で取り直す。
/// `patchLocal` は取り直しを待たずに手元の行を書き換える（改名の直後に古い名前へ戻らないように）。
export function usePinnedThreads(enabled = true): {
  threads: Thread[];
  patchLocal: (id: string, patch: Partial<Thread>) => void;
} {
  const [threads, setThreads] = React.useState<Thread[]>([]);
  const refresh = React.useRef<(() => void) | null>(null);
  useLiveRefresh(threads, refresh);
  React.useEffect(() => {
    if (!enabled) return;
    let active = true;
    // 連続した通知で古い応答が後から届いても上書きしない。
    let latest = 0;
    const load = () => {
      const seq = ++latest;
      listThreads(undefined, { pinned: true })
        .then(({ threads: pinned }) => {
          if (active && seq === latest) setThreads(pinned);
        })
        .catch(() => {
          /* 取れなければ前回の表示を残す（履歴の本体は別に出る） */
        });
    };
    load();
    refresh.current = load;
    const unsubscribe = subscribeThreadsChanged(load);
    return () => {
      active = false;
      refresh.current = null;
      unsubscribe();
    };
  }, [enabled]);
  const patchLocal = React.useCallback((id: string, patch: Partial<Thread>) => {
    setThreads((prev) => prev.map((t) => (t.id === id ? { ...t, ...patch } : t)));
  }, []);
  return { threads, patchLocal };
}
