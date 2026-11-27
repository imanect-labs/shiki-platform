/// 自分のものと共有されたものを合わせた一覧の読み方（`GET /artifacts`・`GET /workflows` の同じ契約）。
///
/// サーバーは先頭のページで自分のものと共有されたものを合わせて返し、それぞれの続きの位置を別に返す
/// （`owned_next`・`shared_next_cursor`）。続きは位置ごとに別の頼みで読む。合わせたページの最後の 1 件を自分のものの
/// 続きの位置に使うと、最後が共有されたものだったときに、それより新しい自分のものを読み飛ばす（BUG-185）。
///
/// 共有されたものが多い人では、共有された分が 0 件のまま続きの位置だけが付いて返ることがある（サーバーが OpenFGA に
/// 問う数を抑えるため・BUG-162）。共有された分は 1 回の読み込みで続きを 5 回までたどり、読み切れなければ位置を残す。

import type { components } from "@/generated/api";

/// 自分のものの続きの位置（前の応答の `owned_next`）。
export type OwnedPosition = { updatedAt: string; id: string };

/// 応答の自分のものの続きの位置（codegen の `OwnedNext`）を、次の頼みに渡す形にする。
export function ownedPosition(next: components["schemas"]["OwnedNext"] | null | undefined): OwnedPosition | null {
  return next ? { updatedAt: next.before_updated_at, id: next.before_id } : null;
}

/// サーバーの 1 ページ。
export type ListPage<T> = {
  items: T[];
  ownedNext: OwnedPosition | null;
  sharedNext: string | null;
};

/// 1 ページの頼み。どちらも無ければ先頭のページ。`before` は自分のものの続き、`sharedCursor` は共有された分の続き。
export type PageRequest = { before?: OwnedPosition; sharedCursor?: string };

export type FetchPage<T> = (req: PageRequest) => Promise<ListPage<T>>;

/// 続きの頼みのどれが失敗したか（自分のものだけ `owned`・共有された分だけ `shared`・両方 `both`）。
export type MoreFailed = "owned" | "shared" | "both" | null;

/// 読めた一覧と、まだ読んでいない続きの位置。
export type OwnedSharedList<T> = {
  items: T[];
  /// 自分のものの続きの位置（読み切ったら `null`）。
  ownedNext: OwnedPosition | null;
  /// 共有された分の続きの位置（読み切ったら `null`）。
  sharedCursor: string | null;
  /// 続きの頼みが失敗した（失敗した側の位置は残すので、もう一度読める）。
  failed: MoreFailed;
};

/// 並べ替えと重ね合わせに使う欄（アーティファクトもワークフローの要約も持つ）。
export type Listed = { id: string; updatedAt: string };

/// 共有された分の続きを、1 回の読み込みでたどる回数の上限。
const SHARED_MAX_PAGES = 5;

/// 先頭のページを読む。共有された分は続きを 5 回までたどる。最初の頼みの失敗は投げ、続きの頼みの失敗は、そこまでの
/// 分と印を返す（自分のものまで捨てない）。
export async function readFirst<T extends Listed>(fetchPage: FetchPage<T>): Promise<OwnedSharedList<T>> {
  const first = await fetchPage({});
  const shared = await readShared(fetchPage, first.sharedNext);
  return {
    items: mergeNewest(first.items, shared.items),
    ownedNext: first.ownedNext,
    sharedCursor: shared.cursor,
    failed: shared.failed ? "shared" : null,
  };
}

/// 残っている続きを読む（自分のものは 1 ページ、共有された分は 5 回まで）。返す `items` は読み足した分だけ
/// （前の分とは [`mergeNewest`] で合わせる）。失敗した側は位置をそのまま返す。
export async function readMore<T extends Listed>(
  fetchPage: FetchPage<T>,
  from: Pick<OwnedSharedList<T>, "ownedNext" | "sharedCursor">,
): Promise<OwnedSharedList<T>> {
  const [owned, shared] = await Promise.all([
    from.ownedNext
      ? fetchPage({ before: from.ownedNext }).then(
          (p) => ({ items: p.items, next: p.ownedNext, failed: false }),
          () => ({ items: [] as T[], next: from.ownedNext, failed: true }),
        )
      : { items: [] as T[], next: null, failed: false },
    readShared(fetchPage, from.sharedCursor),
  ]);
  return {
    items: mergeNewest(owned.items, shared.items),
    ownedNext: owned.next,
    sharedCursor: shared.cursor,
    failed: owned.failed && shared.failed ? "both" : owned.failed ? "owned" : shared.failed ? "shared" : null,
  };
}

async function readShared<T>(
  fetchPage: FetchPage<T>,
  start: string | null,
): Promise<{ items: T[]; cursor: string | null; failed: boolean }> {
  const items: T[] = [];
  let cursor = start;
  for (let page = 0; cursor && page < SHARED_MAX_PAGES; page += 1) {
    let more: ListPage<T>;
    try {
      more = await fetchPage({ sharedCursor: cursor });
    } catch {
      return { items, cursor, failed: true };
    }
    items.push(...more.items);
    cursor = more.sharedNext;
  }
  return { items, cursor, failed: false };
}

/// 2 つの一覧を、重ねずに更新の新しい順で合わせる。
export function mergeNewest<T extends Listed>(a: T[], b: T[]): T[] {
  const seen = new Set<string>();
  return [...a, ...b]
    .filter((m) => (seen.has(m.id) ? false : (seen.add(m.id), true)))
    .sort((x, y) => Date.parse(y.updatedAt) - Date.parse(x.updatedAt) || y.id.localeCompare(x.id));
}
