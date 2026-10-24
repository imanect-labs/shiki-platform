/// 部署（ロール）id → 表示名（同じテナントのディレクトリ・`GET /directory/roles?q=`）。
///
/// データのグリッドの `role_ref` 列・絞り込みの値を、id（`sales`）ではなく表示名（「営業部」）で見せるために
/// 使う。表示名は権限の材料ではないのでタブの中で覚え、ディレクトリに無い id は `null` として覚えて引き直さない。
/// 一括で引く API は無いので id ごとに検索し、id が完全に一致するものを採る（部署は少数なので件数は小さい）。

import { searchRoles } from "@/lib/storage";

const known = new Map<string, string | null>();
const inflight = new Map<string, Promise<void>>();

export function cachedRoleName(id: string): string | null | undefined {
  return known.get(id);
}

/// 候補の検索などで分かった表示名を覚える。
export function rememberRoleName(id: string, name: string) {
  if (id && name) known.set(id, name);
}

/// まだ引いていない id の表示名を引く。新しく名前が分かったら true。
export async function resolveRoleNames(ids: Iterable<string>): Promise<boolean> {
  const todo = [...new Set(ids)].filter((id) => id && !known.has(id) && !inflight.has(id)).slice(0, 50);
  let learned = false;
  await Promise.all(
    todo.map((id) => {
      const p = (async () => {
        try {
          const r = await searchRoles({ q: id, limit: 20 });
          for (const x of r.items) rememberRoleName(x.id, x.display_name);
          if (!known.has(id)) known.set(id, null);
          if (known.get(id)) learned = true;
        } catch {
          // 引けなければ id のまま見せる（次の表示で引き直す）。
        } finally {
          inflight.delete(id);
        }
      })();
      inflight.set(id, p);
      return p;
    }),
  );
  return learned;
}
