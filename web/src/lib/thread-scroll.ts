/// 会話ごとのスクロール位置の記録（#1154）。
///
/// `scrollTop` の数値ではなく「画面の上端にあった行（メッセージ）とその中のずれ」で覚える。画面幅
/// （PC／スマホ・サイドバーの開閉）や、Markdown・数式・画像・引用チップの描画で高さが変わるため、
/// 数値のままでは同じ場所に戻らない。あわせて「最新を見ていたか（下端に付いていたか）」と、
/// そのとき最後にあった行も覚える（離れている間に増えた回答の先頭へ合わせるため）。
///
/// 会話 id を含むユーザーのデータなので、ユーザー単位のキーに置きログアウトで消す（#817）。
/// この端末にだけ置く（端末ごとに画面幅が違い、位置の意味が変わるため同期しない）。

import { scopedStorageKey } from "@/lib/user-storage";

export const THREAD_SCROLL_KEY = "shiki:thread-scroll";
/// 覚えておく会話の数（古いものから捨てる）。
const MAX_THREADS = 200;

export type ThreadScroll = {
  /// 画面の上端にあった行の目印（`data-anchor`）。
  anchor: string | null;
  /// その行の上端から、画面の上端（本文の上の余白の位置）までの距離（px）。
  offset: number;
  /// 下端に付いていた（最新を見ていた）。
  atBottom: boolean;
  /// そのとき最後にあった行の目印（離れている間に増えた分の先頭を見つける）。
  last: string | null;
  savedAt: number;
};

type Store = Record<string, ThreadScroll>;

function load(): { key: string; store: Store } | null {
  const key = scopedStorageKey(THREAD_SCROLL_KEY);
  if (!key || typeof window === "undefined") return null;
  try {
    const v = JSON.parse(window.localStorage.getItem(key) ?? "{}") as unknown;
    return { key, store: v && typeof v === "object" && !Array.isArray(v) ? (v as Store) : {} };
  } catch {
    return { key, store: {} };
  }
}

export function readThreadScroll(threadId: string): ThreadScroll | null {
  const rec = load()?.store[threadId];
  if (!rec || typeof rec !== "object" || typeof rec.offset !== "number") return null;
  return rec;
}

export function writeThreadScroll(threadId: string, rec: Omit<ThreadScroll, "savedAt">): void {
  const loaded = load();
  if (!loaded) return; // ユーザーが確定する前は書かない（前の人のキーへ書かない）
  const store: Store = { ...loaded.store, [threadId]: { ...rec, savedAt: Date.now() } };
  const ids = Object.keys(store);
  if (ids.length > MAX_THREADS) {
    ids
      .sort((a, b) => store[a].savedAt - store[b].savedAt)
      .slice(0, ids.length - MAX_THREADS)
      .forEach((id) => delete store[id]);
  }
  try {
    window.localStorage.setItem(loaded.key, JSON.stringify(store));
  } catch {
    // 容量超過等は無視する（覚えられないだけで、会話は普通に開ける）。
  }
}
