/// ユーザー id → 表示名（同じテナントのディレクトリ・`POST /directory/users/names`）。
///
/// データのグリッドの `user_ref` 列と作成者列を、生の id ではなく表示名で見せるために使う。引いた名前は
/// タブの中で覚え（ディレクトリの表示名は権限の材料ではない）、ディレクトリに居ない id は `null` として
/// 覚えて引き直さない（呼び出し側は id のまま見せる）。

import { apiFetch } from "@/lib/api";
import type { components } from "@/generated/api";

type ResolveUserNamesResponse = components["schemas"]["ResolveUserNamesResponse"];

/// 1 回の要求で引く id の上限（サーバの `MAX_RESOLVE_USER_NAMES` と同じ）。
const BATCH = 200;

const known = new Map<string, string | null>();
const inflight = new Map<string, Promise<void>>();

/// 覚えている表示名（未解決・ディレクトリに居ない id は `undefined` / `null`）。
export function cachedUserName(id: string): string | null | undefined {
  return known.get(id);
}

/// まだ引いていない id の表示名を引く（同じ id の同時要求は 1 回にまとめる）。新しく名前が分かったら true。
export async function resolveUserNames(ids: Iterable<string>): Promise<boolean> {
  const todo = [...new Set(ids)].filter((id) => id && !known.has(id) && !inflight.has(id));
  const waits: Promise<void>[] = [];
  let learned = false;
  for (let i = 0; i < todo.length; i += BATCH) {
    const chunk = todo.slice(i, i + BATCH);
    const p = (async () => {
      try {
        const res = await apiFetch("/directory/users/names", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ ids: chunk }),
        });
        if (!res.ok) return; // 引けなければ id のまま見せる（次の表示で引き直す）。
        const body = (await res.json()) as ResolveUserNamesResponse;
        for (const id of chunk) {
          const name = body.names[id] ?? null;
          known.set(id, name);
          if (name) learned = true;
        }
      } catch {
        // 通信の失敗も id のまま（次の表示で引き直す）。
      } finally {
        for (const id of chunk) inflight.delete(id);
      }
    })();
    for (const id of chunk) inflight.set(id, p);
    waits.push(p);
  }
  await Promise.all(waits);
  return learned;
}
