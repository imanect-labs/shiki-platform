/// storage node id → 表示パス（「経費/規程/出張旅費規程.pdf」）の解決（#919）。
///
/// スキルの知識スコープ・本文のドライブ参照を ID ではなくパスで見せるために使う。
/// パンくず（`GET /nodes/{id}/breadcrumb`）は毎回サーバで閲覧者本人の権限で認可され、
/// 読める接頭だけが返る。方針は [`@/lib/node-name-cache`] と同じ:
/// - 同じ id の並行リクエストは 1 本にまとめる・解決結果は TTL の間だけ使う
/// - 解決できない（削除済み・権限なし）は `null` として記憶し、TTL の間は叩き直さない

import * as React from "react";

import { breadcrumb } from "@/lib/storage";

/// 共有解除・権限失効の後もパスを出し続けないための上限。
const TTL_MS = 60_000;

const resolved = new Map<string, { path: string | null; at: number }>();
const inflight = new Map<string, Promise<void>>();

function fresh(id: string): boolean {
  const hit = resolved.get(id);
  return !!hit && Date.now() - hit.at < TTL_MS;
}

function fetchPath(id: string): Promise<void> {
  const existing = inflight.get(id);
  if (existing) return existing;
  const p = breadcrumb(id)
    .then((crumbs) => {
      const names = crumbs.map((c) => c.name.trim()).filter(Boolean);
      resolved.set(id, { path: names.length > 0 ? names.join("/") : null, at: Date.now() });
    })
    .catch(() => {
      resolved.set(id, { path: null, at: Date.now() });
    })
    .finally(() => {
      inflight.delete(id);
    });
  inflight.set(id, p);
  return p;
}

/// 解決済みのパスを先に入れる（ピッカーで選んだ直後に「読み込み中」を挟まない）。
export function primeNodePath(id: string, path: string): void {
  resolved.set(id, { path, at: Date.now() });
}

/// 与えた id 群のパスを解決する。値は `string`（パス）・`null`（解決できない）・
/// `undefined`（読み込み中）。
export function useNodePaths(ids: readonly string[]): Record<string, string | null | undefined> {
  const key = React.useMemo(() => Array.from(new Set(ids)).sort().join(","), [ids]);
  const [tick, bump] = React.useReducer((n: number) => n + 1, 0);

  React.useEffect(() => {
    if (!key) return;
    let active = true;
    const pending = key.split(",").filter((id) => id && !fresh(id));
    if (pending.length > 0) {
      void Promise.all(pending.map(fetchPath)).then(() => {
        if (active) bump();
      });
    }
    return () => {
      active = false;
    };
  }, [key, tick]);

  return React.useMemo(() => {
    const out: Record<string, string | null | undefined> = {};
    for (const id of key ? key.split(",") : []) {
      const hit = resolved.get(id);
      out[id] = hit && Date.now() - hit.at < TTL_MS ? hit.path : undefined;
    }
    return out;
    // key が変わるか、解決が終わって tick が進んだときだけ作り直す。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, tick]);
}

/// 1 件版（ノードビュー用）。
export function useNodePath(id: string): string | null | undefined {
  const ids = React.useMemo(() => [id], [id]);
  return useNodePaths(ids)[id];
}
