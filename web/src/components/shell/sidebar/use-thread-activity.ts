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

/// 上限を超えた分を、古い既読から捨てる。今回記録した会話（`fresh`）は捨てない
/// （捨てると次の描画でまた記録し直し、記録と描画が止まらなくなる）。
function trim(seen: SeenMap, fresh: ReadonlySet<string>): SeenMap {
  const ids = Object.keys(seen);
  if (ids.length <= MAX_SEEN) return seen;
  const rest = ids
    .filter((id) => !fresh.has(id))
    .sort((a, b) => Date.parse(seen[b]) - Date.parse(seen[a]))
    .slice(0, Math.max(0, MAX_SEEN - fresh.size));
  return Object.fromEntries([...fresh, ...rest].map((id) => [id, seen[id]]));
}

/// 次の既読の記録（変わらなければ `prev` をそのまま返す）。
///
/// 記録は前へしか進めない（手元の一覧が古いタブが、別のタブの新しい既読を古い値で書き戻さない。
/// 書き戻すと storage イベントで互いに読み直し・書き直しを繰り返す）。
function nextSeen(prev: SeenMap, viewing: Thread | undefined, rows: readonly Thread[]): SeenMap {
  const fresh = new Set<string>();
  const next: SeenMap = { ...prev };
  if (viewing) {
    const last = prev[viewing.id];
    if (last === undefined || Date.parse(viewing.updatedAt) > Date.parse(last)) {
      next[viewing.id] = viewing.updatedAt;
      fresh.add(viewing.id);
    }
  }
  for (const t of rows) {
    if (t.runStatus === null || t.id === viewing?.id || prev[t.id] !== undefined) continue;
    next[t.id] = t.updatedAt;
    fresh.add(t.id);
  }
  return fresh.size > 0 ? trim(next, fresh) : prev;
}

/// 履歴の各行の生成状態を返す。
///
/// 実行中・承認待ちはサーバの `runStatus` が正。「終わった（未読）」はこのブラウザで最後に見た
/// 更新日より、サーバの更新日が新しい会話（回答の確定で更新日が進む）:
/// - 今開いている会話は、更新日が変わるたびに既読にする（見ている間に終わった回答は未読にしない）。
/// - 実行中に見かけた会話で記録が無いものは、その時点の更新日を記録する（別の端末・別のタブで
///   始めた生成も、終わったら未読になる）。
///
/// 記録の無い会話（このブラウザで開いたことも、動いているのを見たことも無い）は未読にしない。
/// 時刻はサーバの値どうしで比べる（端末の時計のずれに左右されない）。
/// ユーザーが確定する前（/me の応答前）は読まず書かず、未読も出さない。
export function useThreadActivity(
  viewingId: string | null,
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
    const viewing = viewingId ? rows.find((t) => t.id === viewingId) : undefined;
    const seen = nextSeen(state.seen, viewing, rows);
    if (seen === state.seen) return;
    try {
      window.localStorage.setItem(state.key, JSON.stringify(seen));
    } catch {
      // 容量超過等は無視する（表示はメモリ上の記録で続ける）。
    }
    setState({ key: state.key, seen });
  }, [state, storageKey, viewingId, rows]);

  const seen = state && state.key === storageKey ? state.seen : null;
  return React.useCallback(
    (thread: Thread): ThreadActivity => {
      if (thread.runStatus === "queued" || thread.runStatus === "running") return "running";
      if (thread.runStatus === "waiting_approval") return "waiting";
      if (!seen || thread.id === viewingId) return null;
      const last = seen[thread.id];
      if (last === undefined) return null;
      return Date.parse(thread.updatedAt) > Date.parse(last) ? "done" : null;
    },
    [seen, viewingId],
  );
}
