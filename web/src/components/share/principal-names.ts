"use client";

/// 共有相手（user / role）の表示名の解決。
///
/// 共有一覧 API（ShareEntry）は `{type, id}` しか返さないため、そのままでは OIDC `sub`（UUID）や
/// ロール ID が画面に出てしまう。ここでは:
/// - 共有ダイアログの検索結果で見た名前を覚えておく（同じセッション内の付与直後は必ず引ける）。
/// - ロールはディレクトリ検索が role_id にも一致するので、未知の ID は検索して完全一致を拾う。
/// - ユーザーは id で引く API が無いため、検索で見ていない相手は解決できない（呼び出し側が
///   「名前を取得できないメンバー」として出す）。サーバ側で ShareEntry に表示名を載せるのが本筋。

import * as React from "react";

import { searchRoles } from "@/lib/storage";

export type PrincipalKind = "user" | "role";
export type PrincipalName = { name: string; secondary?: string };

const cache = new Map<string, PrincipalName>();
const inflight = new Set<string>();
const key = (type: PrincipalKind, id: string) => `${type}:${id}`;

export function rememberPrincipal(type: PrincipalKind, id: string, value: PrincipalName): void {
  cache.set(key(type, id), value);
}

export function principalName(type: PrincipalKind, id: string): PrincipalName | undefined {
  return cache.get(key(type, id));
}

/// 一覧に出す相手のうち、未解決のロールを裏で解決する。解決できたら再描画のために値を進める。
export function useResolvePrincipals(targets: { type: PrincipalKind; id: string }[]): number {
  const [version, setVersion] = React.useState(0);
  // 署名は「未解決のロール」だけで作る（問い合わせ中かどうかを混ぜると、問い合わせた直後に
  // 署名が変わって effect が片付けられ、解決後の再描画が落ちる）。
  const signature = targets
    .filter((t) => t.type === "role" && !cache.has(key(t.type, t.id)))
    .map((t) => t.id)
    .join("\n");
  React.useEffect(() => {
    if (!signature) return;
    let active = true;
    // 同じ ID を別のダイアログが問い合わせ中なら重ねて投げない。
    const ids = signature.split("\n").filter((id) => !inflight.has(key("role", id)));
    for (const id of ids) inflight.add(key("role", id));
    Promise.all(
      ids.map((id) =>
        searchRoles({ q: id, limit: 5 })
          .then((res) => {
            const hit = res.items.find((r) => r.id === id);
            if (hit) rememberPrincipal("role", id, { name: hit.display_name, secondary: "部署・ロール" });
          })
          .catch(() => undefined)
          .finally(() => inflight.delete(key("role", id))),
      ),
    ).then(() => {
      if (active) setVersion((v) => v + 1);
    });
    return () => {
      active = false;
    };
  }, [signature]);
  return version;
}
