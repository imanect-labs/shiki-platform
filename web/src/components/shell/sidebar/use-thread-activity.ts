"use client";

import * as React from "react";

import type { Thread } from "@/lib/chat-api";
import { scopedStorageKey, subscribeStorageScope } from "@/lib/user-storage";

/// 履歴の行の右端に出す、裏で動いている（動いていた）生成の状態。
/// `running`＝順番待ち・生成中、`waiting`＝承認待ち、`done`＝開いていない間に生成が終わった（完了・失敗・停止のいずれも・未読）。
export type ThreadActivity = "running" | "waiting" | "done" | null;

/// 既読の記録（会話 id を含むユーザーのデータなので、ユーザー単位のキーに置きログアウトで消す・#817）。
export const THREAD_SEEN_KEY = "shiki:thread-seen";
/// 覚えておく会話の数（古い既読から捨てる。捨てた会話は「未読」を出さないだけで壊れない）。
const MAX_SEEN = 300;

/// 会話 id → 最後に見たときの更新日（サーバの `updatedAt` をそのまま）。
type SeenMap = Record<string, string>;

function parse(raw: string | null): SeenMap {
  if (!raw) return {};
  try {
    const v = JSON.parse(raw) as unknown;
    if (!v || typeof v !== "object" || Array.isArray(v)) return {};
    return Object.fromEntries(
      Object.entries(v as Record<string, unknown>).filter(
        (e): e is [string, string] => typeof e[1] === "string",
      ),
    );
  } catch {
    return {};
  }
}

/// 次の既読の記録（変わらなければ `prev` をそのまま返す）。
///
/// - 記録は前へしか進めない（手元の一覧が古いタブが、別のタブの新しい既読を古い値で書き戻さない。
///   書き戻すと storage イベントで互いに読み直し・書き直しを繰り返す）。
/// - 上限を超えたら**記録した順で古いもの**から捨てる（記録し直した会話は末尾へ移す）。会話の
///   更新日の順で捨てると、古い会話を記録した直後にそれ自身や別のタブが記録した会話を捨て、
///   互いに記録し直し続ける。記録順なら今回・直前に記録した会話は捨てない。
///   （会話 id は UUID なので、オブジェクトのキー順＝挿入順が保たれる。）
function nextSeen(
  prev: SeenMap,
  viewing: readonly Thread[],
  rows: readonly Thread[],
): SeenMap {
  const fresh: [string, string][] = [];
  const viewingIds = new Set(viewing.map((t) => t.id));
  for (const t of viewing) {
    const last = prev[t.id];
    if (last === undefined || Date.parse(t.updatedAt) > Date.parse(last)) {
      fresh.push([t.id, t.updatedAt]);
    }
  }
  for (const t of rows) {
    if (t.runStatus === null || viewingIds.has(t.id) || prev[t.id] !== undefined) continue;
    fresh.push([t.id, t.updatedAt]);
  }
  if (fresh.length === 0) return prev;
  const freshIds = new Set(fresh.map(([id]) => id));
  const kept = Object.entries(prev).filter(([id]) => !freshIds.has(id));
  const all = [...kept, ...fresh];
  return Object.fromEntries(all.slice(Math.max(0, all.length - MAX_SEEN)));
}

/// 履歴の各行の生成状態を返す。
///
/// 実行中・承認待ちはサーバの `runStatus` が正。「終わった（未読）」はこのブラウザで最後に見た
/// 更新日より、サーバの更新日が新しい会話（回答の確定で更新日が進む）:
/// - いま表示している会話（`viewing`）は、更新日が進むたびに既読にする（見ている間に終わった回答は
///   未読にしない）。
/// - 実行中に見かけた会話で記録が無いものは、その時点の更新日を記録する（別の端末・別のタブで
///   始めた生成も、終わったら未読になる）。
///
/// 記録の無い会話（このブラウザで開いたことも、動いているのを見たことも無い）は未読にしない。
/// 時刻はサーバの値どうしで比べる（端末の時計のずれに左右されない）。
/// ユーザーが確定する前（/me の応答前）は読まず書かず、未読も出さない。
export function useThreadActivity(
  viewing: ReadonlySet<string>,
  rows: readonly Thread[],
): (thread: Thread) => ThreadActivity {
  const storageKey = React.useSyncExternalStore(
    subscribeStorageScope,
    () => scopedStorageKey(THREAD_SEEN_KEY),
    () => null,
  );
  // どのキーから読んだ記録か（ユーザーが替わったら読み直す。前の人の記録を次の人のキーへ書かない）。
  const [state, setState] = React.useState<{ key: string; seen: SeenMap } | null>(null);

  React.useEffect(() => {
    if (!storageKey) {
      setState(null);
      return;
    }
    const load = () => {
      let raw: string | null = null;
      try {
        raw = window.localStorage.getItem(storageKey);
      } catch {
        // Web Storage 不可なら、この画面の間だけメモリで持つ。
      }
      setState({ key: storageKey, seen: parse(raw) });
    };
    load();
    // 別のタブで開いて既読にした会話は、こちらでも消す。
    const onStorage = (e: StorageEvent) => {
      if (e.key === storageKey) load();
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, [storageKey]);

  React.useEffect(() => {
    if (!state || state.key !== storageKey) return;
    // 同じ会話が別々に取り直す一覧（ピン留め・通常）に違う更新日で載ることがある。新しい方で既読にする。
    const latestShown = new Map<string, Thread>();
    for (const t of rows) {
      if (!viewing.has(t.id)) continue;
      const prev = latestShown.get(t.id);
      if (!prev || Date.parse(t.updatedAt) > Date.parse(prev.updatedAt)) latestShown.set(t.id, t);
    }
    const shown = [...latestShown.values()];
    const seen = nextSeen(state.seen, shown, rows);
    if (seen === state.seen) return;
    try {
      window.localStorage.setItem(state.key, JSON.stringify(seen));
    } catch {
      // 容量超過等は無視する（表示はメモリ上の記録で続ける）。
    }
    setState({ key: state.key, seen });
  }, [state, storageKey, viewing, rows]);

  const seen = state && state.key === storageKey ? state.seen : null;
  return React.useCallback(
    (thread: Thread): ThreadActivity => {
      if (thread.runStatus === "queued" || thread.runStatus === "running") return "running";
      if (thread.runStatus === "waiting_approval") return "waiting";
      if (!seen || viewing.has(thread.id)) return null;
      const last = seen[thread.id];
      if (last === undefined) return null;
      return Date.parse(thread.updatedAt) > Date.parse(last) ? "done" : null;
    },
    [seen, viewing],
  );
}
